#!/usr/bin/env python3
"""Isolated integration checks: fake stats/manager, only owned child processes."""
import contextlib
import fcntl
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest

TOOL = Path(os.environ['SLOTR_BINARY'])
STUB = r'''#!/usr/bin/env python3
import fcntl, json, os, pathlib, signal, sys
root = pathlib.Path(os.environ['XDG_RUNTIME_DIR'])
args = sys.argv[1:]
lock_path = root / 'slotr/state.lock'
if lock_path.exists():
    with lock_path.open('r') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            for line in pathlib.Path('/proc/locks').read_text().splitlines():
                fields = line.split()
                if len(fields) > 5 and fields[1] == 'FLOCK' and fields[4] == str(os.getppid()) and fields[5].endswith(':' + str(lock_path.stat().st_ino)):
                    print('blocking subprocess under state.lock', file=sys.stderr)
                    sys.exit(9)
name = pathlib.Path(sys.argv[0]).name
if name == 'taskr':
    with (root / 'notes').open('a') as log:
        log.write(json.dumps(dict(args=args, task=os.environ.get('TASKR_TASK'), launch=os.environ.get('TASKR_LAUNCH'))) + '\n')
    sys.exit(int(os.environ.get('NOTE_EXIT', '0')))
if name == 'systemd-run':
    # Admission must keep its shared compatibility lock through unit startup.
    with (root / "legacy.lock").open("a") as legacy:
        try:
            fcntl.flock(legacy, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            pass
        else:
            print("shared legacy lock lost during startup", file=sys.stderr)
            sys.exit(9)
    unit = args[args.index('--unit') + 1]
    if (root / 'collision').exists():
        (root / 'collision').unlink()
        print('Unit already exists', file=sys.stderr)
        sys.exit(1)
    (root / unit).write_text(str(os.getpid()))
    with (root / 'launches').open('a') as log:
        log.write(json.dumps(args[:args.index('--')]) + '\n')
    cmd = args[args.index('--') + 1:]
    os.execv(cmd[0], cmd)
if 'Version' in ' '.join(args):
    if (root / 'no-bus').exists():
        print('Failed to connect to bus: No data available', file=sys.stderr)
        sys.exit(1)
    print('Version=fake')
    sys.exit(0)
unit = args[-1]
path = root / unit
try:
    pid = int(path.read_text())
    os.kill(pid, 0)
    live = pathlib.Path('/proc/%s/stat' % pid).read_text().split()[2] != 'Z'
except (OSError, ValueError):
    live = False
if 'is-active' in args:
    print('active' if live else 'inactive')
    sys.exit(0 if live else 3)
if 'stop' in args:
    if live:
        os.kill(pid, signal.SIGTERM)
    sys.exit(0)
print('/' + unit)
'''
WORKLOAD = r'''
import json, os, pathlib, signal, sys, time
path = pathlib.Path(sys.argv[1])
path.write_text(json.dumps(dict(pid=os.getpid(), slot=os.environ['SLOTR_SLOT'],
                               base=os.environ['SLOTR_PORT_BASE'], run=os.environ['SLOTR_RUN'],
                               literal=sys.argv[2:])))
signal.signal(signal.SIGTERM, lambda s, f: sys.exit(0))
while True:
    time.sleep(0.1)
'''


class AdmissionTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='slotr-test-')
        self.root = Path(self.tmp.name)
        self.runtime = self.root / 'runtime'
        self.proc = self.root / 'proc'
        self.cgroup = self.root / 'cgroup'
        self.bin = self.root / 'bin'
        for path in (self.runtime, self.proc / 'pressure', self.cgroup, self.bin, self.root / 'config/slotr'):
            path.mkdir(parents=True)
        for name in ('systemd-run', 'systemctl', 'taskr'):
            path = self.bin / name
            path.write_text(STUB)
            path.chmod(0o755)
        self.env = dict({k:v for k,v in os.environ.items() if not k.startswith('SLOTR_')}, XDG_RUNTIME_DIR=str(self.runtime), XDG_CONFIG_HOME=str(self.root / 'config'),
                        XDG_STATE_HOME=str(self.root / 'state'), SLOTR_TEST='1',
                        SLOTR_PROC_ROOT=str(self.proc), SLOTR_CGROUP_ROOT=str(self.cgroup),
                        SLOTR_SYSTEMD_RUN=str(self.bin / 'systemd-run'), SLOTR_SYSTEMCTL=str(self.bin / 'systemctl'),
                        PATH=str(self.bin) + ':' + os.environ['PATH'], TASKR_TASK='test-task', TASKR_LAUNCH='test-launch',
                        HERDR_PANE_ID='test-pane', SECRET_SENTINEL='private-test-value')
        self.cfg = dict(queue_poll_ms=40, watchdog_ms=70, term_grace_seconds=0.12,
                        recovery_healthy_seconds=0.3, legacy_runtime_lock=str(self.runtime / 'legacy.lock'),
                        ports={'base': self.free_block(), 'stride': 32})
        self.write_config()
        self.set_stats(20000)
        self.children = []
        self.workloads = []
        self.logs = []

    def free_block(self):
        for base in range(24000, 30000, 64):
            with contextlib.ExitStack() as stack:
                try:
                    for port in range(base, base + 64):
                        stack.enter_context(socket.socket()).bind(('127.0.0.1', port))
                    return base
                except OSError:
                    continue
        self.fail('no scratch port block')

    def write_config(self):
        cfg = dict(pools=dict(runtime=dict(slots=self.cfg.get("runtime_slots", 2),
                   memory_gated=True, evictable=self.cfg.get("evictable", True),
                   default_cost_mib=7680, max_lease=self.cfg.get("max_lease", "4h"),
                   campaign_cap=self.cfg.get("campaign_cap", 1),
                   ports=dict(self.cfg["ports"], probe=7),
                   legacy_lock=dict(path=self.cfg["legacy_runtime_lock"], mode="shared"),
                   kinds={"dev-stack": dict(cost_mib=7680)})),
                   admission=dict(queue_poll_ms=self.cfg["queue_poll_ms"],
                                  recovery_healthy_seconds=self.cfg["recovery_healthy_seconds"]),
                   watchdog=dict(interval_ms=self.cfg["watchdog_ms"],
                                 term_grace_seconds=self.cfg["term_grace_seconds"],
                                 observe_locks=[str(self.runtime / "heavy.lock")]),
                   lease=dict(grace_seconds=self.cfg.get("grace", 0.2),
                              waiter_min_wait_seconds=self.cfg.get("waiter_age", 0.1),
                              on_expiry=self.cfg.get("on_expiry", "stop"),
                              idle_release_minutes=self.cfg.get("idle_release_minutes", 0.0)),
                   hooks=dict(on_admit=self.cfg.get("on_admit", []),
                              on_warn=self.cfg.get("on_warn", []),
                              on_stop=self.cfg.get("on_stop", ["taskr", "note", "{task}", "{reason}"])))
        if "unknown" in self.cfg:
            cfg["unknown"] = self.cfg["unknown"]
        lines = []
        def table(obj, prefix=""):
            if prefix:
                lines.append("[" + prefix + "]")
            for key, value in obj.items():
                if not isinstance(value, dict):
                    lines.append(key + " = " + json.dumps(value))
            for key, value in obj.items():
                if isinstance(value, dict):
                    table(value, prefix + "." + key if prefix else key)
        table(cfg)
        (self.root / "config/slotr/config.toml").write_text("\n".join(lines) + "\n")

    def set_stats(self, available, avg10=0, avg60=0):
        for path, text in ((self.proc / 'meminfo', f'MemAvailable: {available * 1024} kB\n'),
                           (self.proc / 'pressure/memory', f'some avg10=0 avg60=0 avg300=0 total=0\nfull avg10={avg10} avg60={avg60} avg300=0 total=0\n'),
                           (self.proc / 'loadavg', '99.0 0 0 1/1 1\n')):
            pending = path.with_suffix('.tmp')
            pending.write_text(text)
            pending.replace(path)

    def invoke(self, *args, env=None):
        args = list(args)
        if args[0] == "run":
            at = args.index("--")
            args[at:at] = ["--pool", "runtime", "--campaign", "oneshot", "--purpose", "test"]
        return subprocess.run([str(TOOL), *args], env=env or self.env, capture_output=True, text=True, timeout=5)

    def status(self):
        (self.proc / "locks").write_text(Path("/proc/locks").read_text())
        result = self.invoke("status", "--json")
        self.assertEqual(result.returncode, 0, result.stderr)
        data = json.loads(result.stdout)
        pool = data["pools"]["runtime"]
        data["queue"] = pool["queue"]
        data["budget"] = pool["budget"]
        data["slots"] = pool["holders"]
        if not data["slots"]:
            data["slots"] = [dict(state="free", slot=0)]
        return data

    def wait(self, condition, timeout=6):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            value = condition()
            if value:
                return value
            threading.Event().wait(0.04)
        self.fail('condition timed out; logs: ' + ' '.join(p.read_text() for p in self.logs))

    def start(self, label, *literal, campaign=None, lease=None, task='test-task', pane='test-pane'):
        output = self.root / label
        log = self.root / (label + '.log')
        self.logs.append(log)
        self.workloads.append(output)
        with log.open('w') as stream:
            child = subprocess.Popen([str(TOOL), 'run', '--pool', 'runtime', '--kind', 'dev-stack', '--campaign', campaign or label,
                                      '--purpose', label, '--task', task, '--pane', pane,
                                      *(['--lease', lease] if lease else []), '--',
                                      sys.executable, '-c', WORKLOAD, str(output), *literal], env=self.env,
                                     stdin=subprocess.DEVNULL, stdout=stream, stderr=stream)
        self.children.append(child)
        return child, output

    def running(self, label):
        return next((s for s in self.status()['slots'] if s.get('campaign') == label and s['state'] in ('running', 'overdue', 'yielding', 'warned')), None)

    def admitted(self, label, output):
        self.wait(output.exists)
        return self.wait(lambda: self.running(label))

    def anon(self, holder, mib):
        path = self.cgroup / holder['run']
        path.mkdir(exist_ok=True)
        (path / 'memory.stat').write_text(f'anon {mib * 1024 * 1024}\nfile 999999999999\n')

    def tearDown(self):
        for child in self.children:
            if child.poll() is None:
                child.terminate()
        for child in self.children:
            try:
                child.wait(timeout=3)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
        # Only groups created by this test's workload script are signalled.
        for path in self.workloads:
            if path.exists():
                try:
                    os.killpg(json.loads(path.read_text())['pid'], signal.SIGKILL)
                except ProcessLookupError:
                    pass
        self.tmp.cleanup()

    def test_budget_fifo_and_port_exports(self):
        first, p1 = self.start('first', '$LITERAL', 'with spaces')
        h1 = self.admitted('first', p1)
        self.assertEqual(self.status()['budget']['outstanding_mib'], 7680)
        second, p2 = self.start('second')
        self.wait(lambda: len(self.status()['queue']) == 1)
        third, p3 = self.start('third')
        self.wait(lambda: len(self.status()['queue']) == 2)
        state = self.status()
        self.assertEqual([q['campaign'] for q in state['queue']], ['second', 'third'])
        self.assertEqual(state['queue'][0]['wait_reason'], 'memory_budget')
        self.assertFalse(p2.exists())
        self.set_stats(20479)
        self.assertEqual(self.status()['queue'][0]['wait_reason'], 'memory_budget')
        self.set_stats(20480)
        h2 = self.admitted('second', p2)
        self.assertFalse(p3.exists())
        self.assertEqual(self.status()['queue'][0]['wait_reason'], 'no_slot')
        self.assertEqual(abs(h1['port_base'] - h2['port_base']), 32)
        for h, path in ((h1, p1), (h2, p2)):
            payload = json.loads(path.read_text())
            self.assertEqual(int(payload['base']), h['port_base'])
            self.assertEqual(int(payload['slot']), h['slot'])
            self.assertEqual(payload['run'], h['run'])
        self.assertEqual(json.loads(p1.read_text())['literal'], ['$LITERAL', 'with spaces'])
        flags = json.loads((self.runtime / 'launches').read_text().splitlines()[0])
        for flag in ('--collect', '--wait', '--pipe', '--expand-environment=no', '--property=KillMode=control-group',
                     '--property=Restart=no', '--property=MemoryAccounting=yes', '--property=OOMScoreAdjust=500', '--property=OOMPolicy=kill'):
            self.assertIn(flag, flags)
        self.assertNotIn('private-test-value', (self.runtime / 'slotr/state.json').read_text())

    def test_resident_credit_not_file_cache(self):
        _, p1 = self.start('first')
        h1 = self.admitted('first', p1)
        self.anon(h1, 1000)
        self.set_stats(19479)
        _, p2 = self.start('second')
        self.wait(lambda: bool(self.status()['queue']))
        self.assertFalse(p2.exists())
        self.assertEqual(self.status()['budget']['outstanding_mib'], 6680)
        self.set_stats(19480)
        self.admitted('second', p2)

    def test_dead_waiter_does_not_block_fifo(self):
        self.set_stats(10000)
        first, p1 = self.start('first')
        self.wait(lambda: len(self.status()['queue']) == 1)
        _, p2 = self.start('second')
        self.wait(lambda: len(self.status()['queue']) == 2)
        first.kill()
        first.wait(timeout=2)
        self.set_stats(20000)
        self.admitted('second', p2)
        self.assertFalse(p1.exists())
        self.assertEqual(self.status()['queue'], [])
        self.assertFalse((self.runtime / 'slotr/ticket-1').exists())

    def test_ports_busy_then_free_slot(self):
        with socket.socket() as occupied:
            occupied.bind(('127.0.0.1', self.cfg['ports']['base']))
            _, output = self.start('skip')
            h = self.admitted('skip', output)
            self.assertEqual(h['slot'], 1)
        self.children[0].terminate()
        self.children[0].wait(timeout=3)
        self.cfg['runtime_slots'] = 1
        self.write_config()
        with socket.socket() as occupied:
            occupied.bind(('127.0.0.1', self.cfg['ports']['base']))
            _, output = self.start('busy')
            self.wait(lambda: bool(self.status()['queue']))
            self.assertEqual(self.status()['queue'][0]['wait_reason'], 'ports_busy')
            self.assertFalse(output.exists())
        self.admitted('busy', output)

    def test_legacy_exclusive_lock(self):
        with open(self.cfg['legacy_runtime_lock'], 'w') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            _, output = self.start('legacy')
            self.wait(lambda: bool(self.status()['queue']))
            queue = self.status()['queue'][0]
            self.assertEqual(queue['wait_reason'], 'legacy_lock')
            self.assertEqual(queue['legacy_holder_pid'], os.getpid())
            self.assertFalse(output.exists())
        h = self.admitted('legacy', output)
        with open(self.cfg['legacy_runtime_lock'], 'r') as lock:
            with self.assertRaises(BlockingIOError):
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        self.assertEqual(h['slot'], 0)

    def check_stop(self, available, avg10, expected, two=False):
        self.set_stats(30000)
        old, p1 = self.start('old')
        self.admitted('old', p1)
        victim = old
        if two:
            victim, p2 = self.start('new')
            self.admitted('new', p2)
        decoy = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'])
        self.children.append(decoy)
        self.set_stats(available, avg10)
        self.wait(lambda: victim.poll() is not None)
        self.assertEqual(victim.returncode, 75)
        if two:
            self.assertIsNone(old.poll())
        self.assertIsNone(decoy.poll())
        events = [json.loads(line) for line in (self.root / 'state/slotr/events.jsonl').read_text().splitlines()]
        stops = [e for e in events if e['event'] == 'stop']
        self.assertEqual(len(stops), 1)
        self.assertEqual(stops[0]['reason'], expected)
        self.assertEqual(stops[0]['run'], 'slotr-runtime-2' if two else 'slotr-runtime-1')
        self.assertEqual(len(stops[0]['holders']), 2 if two else 1)
        self.assertIn('anon_mib', stops[0]['holders'][0])
        self.assertFalse(stops[0]['observed_locks'][str(self.runtime / 'heavy.lock')])
        self.assertNotIn('private-test-value', json.dumps(events))
        notes = [json.loads(line) for line in (self.runtime / 'notes').read_text().splitlines()]
        self.assertEqual(len(notes), 1)
        self.assertEqual(notes[0]['task'], 'test-task')
        self.assertEqual(notes[0]['launch'], 'test-launch')
        self.assertEqual(events[-1]['exit'], int(self.env.get('NOTE_EXIT', 0)))
        return events

    def test_watchdog_only_newest_and_recovery(self):
        self.check_stop(3000, 0, 'stop_available_mib', two=True)
        _, output = self.start('replacement')
        self.wait(lambda: bool(self.status()['queue']))
        self.assertEqual(self.status()['queue'][0]['wait_reason'], 'recovery')
        self.assertFalse(output.exists())
        self.set_stats(30000)
        self.admitted('replacement', output)

    def test_psi_stop(self):
        self.check_stop(30000, 20, 'stop_psi_full_avg10')

    def test_emergency_stop_note_failure_not_retried(self):
        self.env['NOTE_EXIT'] = '6'
        self.check_stop(1999, 0, 'emergency_available_mib')

    def test_missing_psi_and_admission_threshold(self):
        (self.proc / 'pressure/memory').unlink()
        _, output = self.start('psi')
        self.wait(lambda: bool(self.status()['queue']))
        self.assertEqual(self.status()['queue'][0]['wait_reason'], 'psi')
        self.set_stats(20000, avg60=5.01)
        self.assertEqual(self.status()['queue'][0]['wait_reason'], 'psi')
        self.assertFalse(output.exists())
        self.set_stats(20000, avg60=5)
        self.admitted('psi', output)

    def test_reclaim_gone_unit_and_normal_exit(self):
        result = self.invoke('run', '--kind', 'dev-stack', '--', sys.executable, '-c', 'import sys; sys.exit(17)')
        self.assertEqual(result.returncode, 17, result.stderr)
        _, output = self.start('after')
        h = self.admitted('after', output)
        self.assertEqual(h['slot'], 0)
        self.assertEqual(h['admit_seq'], 2)
        state = json.loads((self.runtime / 'slotr/state.json').read_text())
        self.assertEqual(len(state['holders']), 1)

    def test_corrupt_config_unknown_kind_and_no_bus(self):
        self.cfg['unknown'] = 1
        self.write_config()
        result = self.invoke('status', '--json')
        self.assertEqual(result.returncode, 2)
        self.assertIn('unknown', result.stderr)
        del self.cfg['unknown']
        self.cfg['runtime_slots'] = True
        self.write_config()
        self.assertIn('slots', self.invoke('status').stderr)
        self.cfg['runtime_slots'] = 2
        self.write_config()
        result = self.invoke('run', '--kind', 'wrong', '--', 'true')
        self.assertEqual(result.returncode, 2)
        self.assertFalse((self.runtime / 'slotr').exists())
        (self.runtime / 'no-bus').touch()
        result = self.invoke('run', '--kind', 'dev-stack', '--', 'true')
        self.assertEqual(result.returncode, 2)
        self.assertIn('no systemd user bus here; run it in a terminal', result.stderr)
        root = self.runtime / 'slotr'
        root.mkdir()
        (root / 'state.json').write_text('{broken')
        result = self.invoke('status', '--json')
        self.assertEqual(result.returncode, 2)
        self.assertIn(str(root / 'state.json'), result.stderr)
        self.assertEqual((root / 'state.json').read_text(), '{broken')

    def test_unit_collision_and_readonly_status(self):
        (self.proc / 'locks').write_text(Path('/proc/locks').read_text())
        before = {str(p.relative_to(self.root)): p.read_bytes() for p in self.root.rglob('*') if p.is_file()}
        self.assertEqual(self.status()['slots'][0]['state'], 'free')
        after = {str(p.relative_to(self.root)): p.read_bytes() for p in self.root.rglob('*') if p.is_file()}
        before.pop('proc/locks', None)
        after.pop('proc/locks', None)
        self.assertEqual(before, after)
        (self.runtime / 'collision').touch()
        _, output = self.start('collision')
        h = self.admitted('collision', output)
        self.assertEqual(h['admit_seq'], 2)
        self.assertEqual((self.runtime / 'slotr').stat().st_mode & 0o777, 0o700)

    def events(self, kind):
        path = self.root / "state/slotr/events.jsonl"
        return [e for line in path.read_text().splitlines() if (e := json.loads(line))["event"] == kind] if path.exists() else []

    def advance(self, seconds):
        path = self.root / "clock"
        offset = float(path.read_text()) if path.exists() else 0
        path.write_text(str(offset + seconds))
        self.env["SLOTR_CLOCK_OFFSET_FILE"] = str(path)

    def configure_clock(self):
        self.env["SLOTR_CLOCK_OFFSET_FILE"] = str(self.root / "clock")
        (self.root / "clock").write_text("0")

    def test_campaign_cap_and_yield(self):
        self.set_stats(40000)
        first, p1 = self.start("first", campaign="A")
        self.wait(p1.exists)
        second, p2 = self.start("second", campaign="A")
        self.wait(p2.exists) # uncontended campaign may fill the pool
        _, waiter = self.start("waiter", campaign="B")
        self.wait(lambda: bool(self.status()["queue"]))
        self.wait(lambda: second.poll() is not None)
        self.assertEqual(second.returncode, 75)
        self.assertIsNone(first.poll())
        self.wait(waiter.exists)
        self.assertEqual([e["reason"] for e in self.events("stop")], ["campaign_yield"])

    def test_campaign_cap_skips_only_capped_head(self):
        self.set_stats(40000)
        first, p1 = self.start("first", campaign="A")
        self.wait(p1.exists)
        self.set_stats(10000)
        second, p2 = self.start("second", campaign="A")
        self.wait(lambda: len(self.status()["queue"]) == 1)
        third, p3 = self.start("third", campaign="B")
        self.wait(lambda: len(self.status()["queue"]) == 2)
        self.wait(lambda: self.status()["queue"][0]["wait_reason"] == "campaign_cap")
        self.assertFalse(p2.exists())
        self.set_stats(40000)
        self.wait(p3.exists)
        self.assertFalse(p2.exists()) # no free slot until B releases
        third.terminate()
        third.wait(timeout=3)
        self.wait(p2.exists) # no other campaign waits: A2 retains its place
        self.assertIsNone(first.poll())
        self.assertIsNone(second.poll())

    def test_overdue_without_waiter_continues(self):
        self.configure_clock()
        child, path = self.start("old", lease="1s")
        self.wait(path.exists)
        self.advance(10)
        self.wait(lambda: self.status()["slots"][0]["state"] == "overdue")
        self.assertIsNone(child.poll())
        self.assertEqual(self.events("stop"), [])
        self.assertEqual(self.events("warn"), [])

    def test_unhelpful_expiry_warns_once(self):
        self.configure_clock()
        old, path = self.start("old", lease="1s")
        self.wait(path.exists)
        self.set_stats(10000)
        _, waiter = self.start("waiter")
        self.wait(lambda: bool(self.status()["queue"]))
        self.advance(10)
        self.wait(lambda: bool(self.events("warn")))
        before = len(self.events("warn"))
        # Observe several watchdog samples by advancing real workload CPU stats.
        self.wait(lambda: self.status()["slots"][0]["state"] == "overdue")
        subprocess.run([sys.executable, "-c", "import time; time.sleep(0.4)"], check=True)
        self.assertEqual(len(self.events("warn")), before)
        self.assertEqual(before, 1)
        self.assertIsNone(old.poll())
        self.assertFalse(waiter.exists())
        self.assertEqual(self.events("stop"), [])

    def test_two_overdue_holders_one_waiter_one_stop(self):
        self.configure_clock()
        self.set_stats(40000)
        old, p1 = self.start("old", lease="1s")
        self.wait(p1.exists)
        newer, p2 = self.start("newer", lease="1s")
        self.wait(p2.exists)
        _, waiter = self.start("waiter")
        self.wait(lambda: bool(self.status()["queue"]))
        self.advance(10)
        self.wait(lambda: old.poll() is not None)
        self.assertEqual(old.returncode, 75)
        self.wait(waiter.exists)
        self.assertIsNone(newer.poll())
        self.assertEqual(len(self.events("stop")), 1)

    def test_expiry_only_evicts_holder_that_preserves_effective_head(self):
        self.configure_clock()
        self.set_stats(40000)
        older, p1 = self.start("older", campaign="A", lease="1s")
        self.wait(p1.exists)
        newer, p2 = self.start("newer", campaign="B", lease="1s")
        self.wait(p2.exists)
        _, a2 = self.start("A2", campaign="A")
        self.wait(lambda: len(self.status()["queue"]) == 1)
        _, c = self.start("C", campaign="C")
        self.wait(lambda: len(self.status()["queue"]) == 2)
        self.advance(10)
        self.wait(lambda: newer.poll() is not None)
        self.assertEqual(newer.returncode, 75)
        self.wait(c.exists)
        self.assertIsNone(older.poll())
        self.assertFalse(a2.exists())
        self.assertEqual(len(self.events("stop")), 1)

    def test_expiry_cancelled_when_waiter_leaves(self):
        self.configure_clock()
        self.cfg["grace"] = 20
        self.write_config()
        self.set_stats(40000)
        old, p1 = self.start("old", lease="1s")
        self.wait(p1.exists)
        newer, p2 = self.start("newer")
        self.wait(p2.exists)
        waiter, _ = self.start("waiter")
        self.wait(lambda: bool(self.status()["queue"]))
        self.advance(10)
        self.wait(lambda: any(h["state"] == "warned" for h in self.status()["slots"]))
        waiter.terminate()
        waiter.wait(timeout=3)
        self.wait(lambda: bool(self.events("warn_cancelled")))
        self.advance(30)
        self.assertIsNone(old.poll())
        self.assertIsNone(newer.poll())
        self.assertEqual(self.events("stop"), [])

    def test_warn_expiry_never_stops(self):
        self.configure_clock()
        self.cfg["on_expiry"] = "warn"
        self.write_config()
        self.set_stats(40000)
        old, p1 = self.start("old", lease="1s")
        self.wait(p1.exists)
        _, p2 = self.start("newer")
        self.wait(p2.exists)
        _, _ = self.start("waiter")
        self.wait(lambda: bool(self.status()["queue"]))
        self.advance(10)
        self.wait(lambda: bool(self.events("warn")))
        self.advance(100)
        self.assertIsNone(old.poll())
        self.assertEqual(self.events("stop"), [])

    def test_unevictable_pressure_and_expiry(self):
        self.configure_clock()
        self.cfg["evictable"] = False
        self.write_config()
        old, p1 = self.start("old", lease="1s")
        self.wait(p1.exists)
        self.set_stats(1000)
        _, _ = self.start("waiter")
        self.wait(lambda: bool(self.status()["queue"]))
        self.advance(10000)
        subprocess.run([sys.executable, "-c", "import time; time.sleep(0.4)"], check=True)
        self.assertIsNone(old.poll())
        self.assertEqual(self.events("stop"), [])
        self.assertEqual(self.events("warn"), [])

    def test_idle_release_counts_only_while_waiter_fits(self):
        self.cfg["idle_release_minutes"] = 0.005
        self.cfg["max_lease"] = "0"
        self.write_config()
        self.set_stats(40000)
        old, p1 = self.start("old")
        h1 = self.admitted("old", p1)
        self.anon(h1, 1000)
        (self.cgroup / h1["run"] / "cpu.stat").write_text("usage_usec 0\n")
        subprocess.run([sys.executable, "-c", "import time; time.sleep(0.4)"], check=True)
        self.assertIsNone(old.poll())
        self.assertEqual(self.events("warn"), [])
        _, p2 = self.start("newer")
        self.wait(p2.exists)
        _, waiter = self.start("waiter")
        self.wait(lambda: bool(self.status()["queue"]))
        self.wait(lambda: old.poll() is not None)
        self.assertEqual(old.returncode, 75)
        self.wait(waiter.exists)
        self.assertEqual([e["reason"] for e in self.events("stop")], ["idle_release"])

    def test_hooks_literal_argv_and_empty_task_skip(self):
        self.cfg["on_admit"] = ["taskr", "note", "{purpose}", "{task}", "{pane}", "{run}", "{port_base}", "literal with spaces", "$HOME"]
        self.write_config()
        child, path = self.start("literal purpose")
        self.wait(path.exists)
        notes = self.wait(lambda: (self.runtime / "notes").read_text() if (self.runtime / "notes").exists() else "")
        args = json.loads(notes.splitlines()[0])["args"]
        self.assertEqual(args[1:4], ["literal purpose", "test-task", "test-pane"])
        self.assertEqual(args[-2:], ["literal with spaces", "$HOME"])
        child.terminate()
        child.wait(timeout=3)
        _, path2 = self.start("skipped", task="")
        self.wait(path2.exists)
        self.assertEqual(len((self.runtime / "notes").read_text().splitlines()), 1)

    def test_config_env_sources_and_defaults(self):
        import tomllib
        result = self.invoke("config", "show")
        self.assertEqual(result.returncode, 0, result.stderr)
        data = json.loads(result.stdout)
        self.assertEqual(data["sources"]["pools.runtime.slots"], "file")
        self.assertEqual(data["sources"]["admission.reserve_mib"], "default")
        override = dict(self.env, SLOTR_POOLS__RUNTIME__SLOTS="3", SLOTR_ADMISSION__RESERVE_MIB="123")
        result = self.invoke("config", "show", env=override)
        self.assertEqual(result.returncode, 0, result.stderr)
        data = json.loads(result.stdout)
        self.assertEqual(data["config"]["pools"]["runtime"]["slots"], 3)
        self.assertEqual(data["config"]["admission"]["reserve_mib"], 123)
        self.assertEqual(data["sources"]["pools.runtime.slots"], "env")
        bad = dict(self.env, SLOTR_ADMISSION__SURPRISE="3")
        result = self.invoke("config", "show", env=bad)
        self.assertEqual(result.returncode, 2)
        self.assertIn("admission.surprise", result.stderr)
        (self.root / "config/slotr/config.toml").unlink()
        result = self.invoke("config", "show")
        built_in = json.loads(result.stdout)["config"]
        example = tomllib.loads((Path(__file__).resolve().parents[1] / "config.example.toml").read_text())
        for pool in built_in["pools"].values():
            pool.pop("ports")
            pool.pop("legacy_lock")
            pool.pop("kinds")
        self.assertEqual(built_in, example)
        self.assertEqual(self.invoke("config", "check", str(self.root / "missing")).returncode, 2)

    def test_stop_only_registered_unit_and_test_seams_gated(self):
        self.assertEqual(self.invoke("stop", "unrelated.service").returncode, 2)
        env = dict(self.env, SLOTR_TEST="0")
        result = self.invoke("status", "--json", env=env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotEqual(json.loads(result.stdout)["stats"]["load1"], 99.0)


if __name__ == '__main__':
    unittest.main(verbosity=2, failfast=True)
