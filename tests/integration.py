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

if pathlib.Path(sys.argv[0]).name == 'systemctl':
    if 'is-active' in args and (root / 'ctl-fail').exists():
        print('boom', file=sys.stderr)
        sys.exit(1)
    if 'stop' in args and '--no-block' not in args:
        # Real `systemctl stop` returns only when the stop job has finished.
        import time
        try:
            pid = int((root / args[-1]).read_text())
            os.kill(pid, signal.SIGTERM)
        except (OSError, ValueError):
            sys.exit(0)
        while True:
            try:
                os.kill(pid, 0)
                if pathlib.Path('/proc/%s/stat' % pid).read_text().split()[2] == 'Z':
                    break
            except OSError:
                break
            time.sleep(0.02)
        sys.exit(0)

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
        print('Failed to start transient service unit: Unit %s.service was already loaded or has a fragment file.' % unit, file=sys.stderr)
        sys.exit(1)
    (root / unit).write_text(str(os.getpid()))
    with (root / 'launches').open('a') as log:
        log.write(json.dumps(args[:args.index('--')]) + '\n')
    cmd = args[args.index('--') + 1:]
    if (root / 'detached').exists():
        # Like real systemd-run --wait: the launcher waits; the service runs in its own session.
        import subprocess
        service = subprocess.Popen(cmd, start_new_session=True)
        (root / unit).write_text(str(service.pid))
        (root / 'launcher').write_text(str(os.getpid()))
        sys.exit(service.wait())
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
def terminated(s, f):
    path.with_suffix(".term").write_text("TERM")
    sys.exit(0)
signal.signal(signal.SIGTERM, signal.SIG_IGN if "--ignore-term" in sys.argv[2:] else terminated)
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
                    for port in range(base, base + 128):
                        stack.enter_context(socket.socket()).bind(('127.0.0.1', port))
                    return base
                except OSError:
                    continue
        self.fail('no scratch port block')

    def write_config(self):
        cfg = dict(pools=dict(runtime=dict(slots=self.cfg.get("runtime_slots", 2),
                   memory_gated=self.cfg.get("memory_gated", True), evictable=self.cfg.get("evictable", True),
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
        for path, text in ((self.proc / 'meminfo', f'MemTotal: {self.cfg.get("total_mib", 65536) * 1024} kB\nMemAvailable: {available * 1024} kB\n'),
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

    def start(self, label, *literal, campaign=None, lease=None, task='test-task', pane='test-pane', **popen):
        output = self.root / label
        log = self.root / (label + '.log')
        self.logs.append(log)
        self.workloads.append(output)
        with log.open('w') as stream:
            child = subprocess.Popen([str(TOOL), 'run', '--pool', 'runtime', '--kind', 'dev-stack', '--campaign', campaign or label,
                                      '--purpose', label, '--task', task, '--pane', pane,
                                      *(['--lease', lease] if lease else []), '--',
                                      sys.executable, '-c', WORKLOAD, str(output), *literal], env=self.env,
                                     stdin=subprocess.DEVNULL, stdout=stream, stderr=stream, **popen)
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


    def pause(self, seconds):
        subprocess.run([sys.executable, '-c', 'import time; time.sleep(%r)' % seconds], check=True)

    def notes(self):
        path = self.runtime / 'notes'
        return [json.loads(l)['args'] for l in path.read_text().splitlines()] if path.exists() else []

    # --- item 1/2: wrong stop and lost queue place on a manager or state error
    def test_probe_p01_manager_error_does_not_kill_holder_or_drop_waiter(self):
        self.cfg['runtime_slots'] = 1
        self.write_config()
        holder, p1 = self.start('holder')
        self.admitted('holder', p1)
        waiter, _ = self.start('waiter')
        self.wait(lambda: bool(self.status()['queue']))
        (self.runtime / 'ctl-fail').touch()          # one failing `systemctl is-active`
        self.pause(1.0)
        (self.runtime / 'ctl-fail').unlink()
        seen = dict(holder_rc=holder.poll(), waiter_rc=waiter.poll(), stop_events=len(self.events('stop')),
                    on_stop_hook_runs=len(self.notes()), holder_log=(self.root / 'holder.log').read_text().strip().splitlines()[-1:],
                    waiter_log=(self.root / 'waiter.log').read_text().strip().splitlines()[-1:])
        self.assertEqual((seen['holder_rc'], seen['waiter_rc']), (None, None), seen)

    def test_probe_p02_unreadable_state_does_not_kill_holder(self):
        holder, p1 = self.start('holder')
        self.admitted('holder', p1)
        state = self.runtime / 'slotr/state.json'
        good = state.read_bytes()
        pending = state.with_suffix('.probe')
        pending.write_text('{broken')
        pending.replace(state)
        self.pause(1.0)
        rc = holder.poll()
        pending.write_bytes(good)
        pending.replace(state)
        self.assertIsNone(rc, 'holder workload was terminated: rc=%s' % rc)

    # --- item 2: `slotr stop` and a signalled frontend against a synchronous `systemctl stop`
    def test_probe_p03_stop_command_reports_success(self):
        self.cfg['term_grace_seconds'] = 6          # default is 15; anything above ctl's 5 s shows it
        self.write_config()
        (self.runtime / 'sync-stop').touch()
        holder, p1 = self.start('holder')
        h = self.admitted('holder', p1)
        began = time.monotonic()
        result = subprocess.run([str(TOOL), 'stop', h['run']], env=self.env, capture_output=True, text=True, timeout=40)
        took = time.monotonic() - began
        holder.wait(timeout=20)
        self.assertEqual(result.returncode, 0, 'after %.1fs: %s' % (took, result.stderr.strip()))

    def test_probe_p04_sigterm_frontend_relays_143(self):
        self.cfg['term_grace_seconds'] = 6
        self.write_config()
        (self.runtime / 'sync-stop').touch()
        holder, p1 = self.start('holder')
        self.admitted('holder', p1)
        holder.terminate()
        holder.wait(timeout=30)
        self.assertEqual(holder.returncode, 143, (self.root / 'holder.log').read_text()[-300:])

    def test_sighup_frontend_stops_unit_and_releases_holder(self):
        holder, p1 = self.start('holder')
        self.admitted('holder', p1)
        holder.send_signal(signal.SIGHUP)
        holder.wait(timeout=30)
        self.assertEqual(holder.returncode, 129, (self.root / 'holder.log').read_text()[-300:])
        self.assertTrue(p1.with_suffix('.term').exists())
        self.assertIsNone(self.running('holder'))
        self.assertEqual(self.status()['pools']['runtime']['holders'], [])

    def test_group_hup_after_launcher_exit_still_stops_detached_unit(self):
        (self.runtime / 'detached').touch()
        holder, p1 = self.start('holder', start_new_session=True)
        self.admitted('holder', p1)
        launcher = int((self.runtime / 'launcher').read_text())
        os.kill(holder.pid, signal.SIGSTOP)           # the launcher handles the tab's HUP first
        os.killpg(holder.pid, signal.SIGHUP)
        self.wait(lambda: Path('/proc/%d/stat' % launcher).read_text().split()[2] == 'Z')
        os.kill(holder.pid, signal.SIGCONT)
        holder.wait(timeout=30)
        self.assertEqual(holder.returncode, 129, (self.root / 'holder.log').read_text()[-300:])
        self.assertTrue(p1.with_suffix('.term').exists())
        self.assertEqual(self.status()['pools']['runtime']['holders'], [])

    def test_inherited_hup_ignore_is_kept(self):
        holder, p1 = self.start('holder', preexec_fn=lambda: signal.signal(signal.SIGHUP, signal.SIG_IGN))
        self.admitted('holder', p1)
        holder.send_signal(signal.SIGHUP)
        time.sleep(0.5)
        self.assertIsNone(holder.poll())
        self.assertFalse(p1.with_suffix('.term').exists())
        self.assertIsNotNone(self.running('holder'))

    # --- lead's smoke: A overdue, A2 yielding (same campaign), one waiter B
    def lead_smoke(self, ignore_term=False):
        self.configure_clock()
        self.cfg['on_warn'] = ['taskr', 'note', '{task}', 'WARN', '{run}', '{reason}']
        self.write_config()
        self.set_stats(40000)
        first, p1 = self.start('first', *(['--ignore-term'] if ignore_term else []), campaign='A', lease='1s')
        self.wait(p1.exists)
        second, p2 = self.start('second', campaign='A')
        self.wait(p2.exists)
        self.advance(10)                              # A is overdue before B arrives; A2 yields once B waits
        _, waiter = self.start('waiter', campaign='B')
        self.wait(lambda: bool(self.status()['queue']))
        return first, second, waiter

    def test_probe_p05_one_waiter_one_warn(self):
        first, second, waiter = self.lead_smoke()
        self.wait(lambda: first.poll() is not None)
        self.wait(waiter.exists)
        self.assertEqual(first.returncode, 75)
        self.assertIsNone(second.poll())
        warns = [(e['run'], e['reason']) for e in self.events('warn')]
        hooks = [a for a in self.notes() if 'WARN' in a]
        self.assertEqual(len(warns), 1, 'warn events: %s; on_warn hook runs: %s' % (warns, hooks))
        self.assertEqual(len(hooks), 1)

    def test_probe_p06_status_shows_stopping_during_term_grace(self):
        self.cfg['term_grace_seconds'] = 1.5
        first, second, waiter = self.lead_smoke(ignore_term=True)
        self.wait(lambda: bool(self.events('stop')))
        run = self.events('stop')[0]['run']
        seen = [s.get('state') for s in self.status()['slots'] if s.get('run') == run]
        self.assertEqual(seen, ['stopping'])

    def test_probe_p07_one_warn_while_fit_flaps_during_grace(self):
        self.configure_clock()
        self.cfg['grace'] = 30
        self.write_config()
        self.set_stats(40000)
        old, p1 = self.start('old', lease='1s')
        self.wait(p1.exists)
        newer, p2 = self.start('newer')
        self.wait(p2.exists)
        _, waiter = self.start('waiter')
        self.wait(lambda: bool(self.status()['queue']))
        self.advance(10)
        self.wait(lambda: bool(self.events('warn')))
        for _ in range(5):                            # the waiter stops fitting for one sample, then fits again
            self.set_stats(10000)
            self.pause(0.25)
            self.set_stats(40000)
            self.pause(0.25)
        warns = [e['reason'] for e in self.events('warn')]
        self.assertIsNone(old.poll())
        self.assertEqual(len(warns), 1, 'warn events %s, cancels %d' % (warns, len(self.events('warn_cancelled'))))

    # --- item 1: a stop for a waiter that cannot admit itself
    def test_probe_p08_frozen_head_waiter_earns_no_stop(self):
        self.configure_clock()
        self.set_stats(40000)
        old, p1 = self.start('old', lease='60s')
        self.wait(p1.exists)
        newer, p2 = self.start('newer')
        self.wait(p2.exists)
        waiter, out = self.start('waiter')
        self.wait(lambda: bool(self.status()['queue']))
        os.kill(waiter.pid, signal.SIGSTOP)           # Ctrl-Z in the waiter's pane
        try:
            self.advance(70)
            self.pause(3.0)
            self.assertIsNone(old.poll(), 'holder stopped (rc=%s) for a frozen waiter; waiter admitted=%s' % (old.returncode, out.exists()))
        finally:
            os.kill(waiter.pid, signal.SIGCONT)

    # --- item 4: the stop side ignores the campaign cap
    def test_probe_p09_no_eviction_for_a_campaign_already_at_cap(self):
        self.configure_clock()
        self.set_stats(40000)
        a, p1 = self.start('a1', campaign='A', lease='1s')
        self.wait(p1.exists)
        b, p2 = self.start('b1', campaign='B')
        self.wait(p2.exists)
        _, b2 = self.start('b2', campaign='B')
        self.wait(lambda: bool(self.status()['queue']))
        self.advance(10)
        self.pause(3.0)
        self.assertIsNone(a.poll(), 'A stopped (rc=%s) so that B holds both slots; b2 admitted=%s' % (a.returncode, b2.exists()))

    def test_probe_p10_free_slot_with_two_mutually_capped_waiters(self):
        self.cfg['runtime_slots'] = 3
        self.write_config()
        self.set_stats(60000)
        _, p1 = self.start('a1', campaign='A')
        self.wait(p1.exists)
        _, p2 = self.start('b1', campaign='B')
        self.wait(p2.exists)
        self.set_stats(10000)
        _, a2 = self.start('a2', campaign='A')
        self.wait(lambda: len(self.status()['queue']) == 1)
        _, b2 = self.start('b2', campaign='B')
        self.wait(lambda: len(self.status()['queue']) == 2)
        self.set_stats(60000)
        self.pause(1.0)
        reasons = [q['wait_reason'] for q in self.status()['queue']]
        self.assertTrue(a2.exists() or b2.exists(), 'one slot free, nobody admitted; wait reasons %s' % reasons)

    # --- item 1 control: on_pressure warn/off (expected to hold)
    def pressure(self, policy):
        self.env['SLOTR_WATCHDOG__ON_PRESSURE'] = policy
        self.cfg['on_warn'] = ['taskr', 'note', '{task}', 'WARN', '{reason}']
        self.write_config()
        holder, p1 = self.start('holder')
        self.admitted('holder', p1)
        self.set_stats(1000)
        self.pause(0.8)
        self.assertIsNone(holder.poll())
        self.assertEqual(self.events('stop'), [])
        return len(self.events('warn'))

    def test_probe_p11_on_pressure_warn_never_stops_and_warns_once(self):
        self.assertEqual(self.pressure('warn'), 1)

    def test_probe_p12_on_pressure_off_never_stops_or_warns(self):
        self.assertEqual(self.pressure('off'), 0)

    # --- item 6 / 2
    def test_probe_p13_workload_stderr_cannot_fake_a_bus_error(self):
        code = "import sys; print('worker: Failed to connect to message bus, retrying', file=sys.stderr); sys.exit(0)"
        result = self.invoke('run', '--kind', 'dev-stack', '--', sys.executable, '-c', code)
        self.assertEqual(result.returncode, 0, result.stderr.strip().splitlines()[-1:])

    def test_probe_p14_non_utf8_environment_value(self):
        env = {os.fsencode(k): os.fsencode(v) for k, v in self.env.items()}
        env[b'LEGACY_LATIN1'] = b'caf\xe9'
        result = subprocess.run([str(TOOL), 'config', 'show'], env=env, capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr.decode(errors='replace').strip()[:200])

    def test_probe_p15_queued_waiter_leaves_the_legacy_lock_free(self):
        self.set_stats(10000)                         # nothing runs; the waiter queues on memory_budget
        self.start('waiter')
        self.wait(lambda: bool(self.status()['queue']))
        got = 0
        with open(self.cfg['legacy_runtime_lock'], 'a') as lock:
            for _ in range(100):
                try:
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    fcntl.flock(lock, fcntl.LOCK_UN)
                    got += 1
                except BlockingIOError:
                    pass
                self.pause(0.01)
        self.assertGreater(got, 50, 'an exclusive legacy user got the lock in %d of 100 tries while only a waiter existed' % got)

    def test_probe_p16_collision_retry_with_the_systemd_255_message(self):
        (self.runtime / 'collision').touch()
        child, output = self.start('collision')
        try:
            self.wait(output.exists, timeout=3)
        except AssertionError:
            state = json.loads((self.runtime / 'slotr/state.json').read_text())
            self.fail('no retry: frontend rc=%s, holder records left=%s, log=%s' % (
                child.poll(), [(h['run'], h['started']) for h in state['holders']], (self.root / 'collision.log').read_text().strip()[-200:]))

    def test_pressure_stop_precedes_slow_hook(self):
        self.env["SLOTR_WATCHDOG__ON_PRESSURE"] = "stop"
        self.cfg["on_stop"] = [sys.executable, "-c", "import pathlib,sys,time; pathlib.Path(sys.argv[1]).touch(); time.sleep(0.8)", str(self.root / "hook-started")]
        self.write_config()
        holder, output = self.start("holder")
        self.admitted("holder", output)
        self.set_stats(1000)
        self.wait(lambda: (self.root / "hook-started").exists())
        self.wait(lambda: output.with_suffix(".term").exists(), timeout=0.5)
        self.assertIsNone(holder.poll(), "hook must still be running when TERM is observed")
        holder.wait(timeout=5)
        self.assertEqual(holder.returncode, 75)

    def test_event_write_failure_does_not_kill_workload(self):
        self.env["SLOTR_WATCHDOG__ON_PRESSURE"] = "warn"
        holder, output = self.start("holder")
        self.admitted("holder", output)
        self.env["SLOTR_WATCHDOG__ON_PRESSURE"] = "warn"
        # Fill the event path with a directory; all future event appends fail.
        path = self.root / "state/slotr/events.jsonl"
        path.unlink()
        path.mkdir()
        self.set_stats(1000)
        self.pause(0.5)
        self.assertIsNone(holder.poll())
        path.rmdir()

    def test_cleanup_failure_preserves_workload_exit(self):
        code = "import pathlib,os,sys; pathlib.Path(os.environ['XDG_RUNTIME_DIR']+'/slotr/state.json').write_text('{broken'); sys.exit(17)"
        result = self.invoke("run", "--kind", "dev-stack", "--", sys.executable, "-c", code)
        self.assertEqual(result.returncode, 17, result.stderr)

    def test_failed_start_removes_own_holder(self):
        stub = (self.bin / "systemd-run").read_text()
        (self.bin / "systemd-run").write_text(stub.replace("os.execv(cmd[0], cmd)", "sys.exit(23)"))
        result = self.invoke("run", "--kind", "dev-stack", "--", "true")
        self.assertEqual(result.returncode, 23, result.stderr)
        state = json.loads((self.runtime / "slotr/state.json").read_text())
        self.assertEqual(state["holders"], [])

    def test_environment_names_only_and_invalid_name_skipped(self):
        self.env["BASH_FUNC_x%%"] = "() { :; }"
        child, output = self.start("holder")
        self.admitted("holder", output)
        flags = json.loads((self.runtime / "launches").read_text().splitlines()[0])
        env_flags = [s for s in flags if s.startswith("--setenv=")]
        self.assertIn("--setenv=SECRET_SENTINEL", env_flags)
        self.assertFalse(any("private-test-value" in s for s in flags))
        self.assertFalse(any("BASH_FUNC" in s for s in env_flags))
        self.assertTrue(all("=" not in s[len("--setenv="):] for s in env_flags))
        self.assertIn("--property=TimeoutStopSec=5.12", flags)

    def test_boottime_state_and_optional_field_upgrade(self):
        holder, output = self.start("holder")
        self.admitted("holder", output)
        path = self.runtime / "slotr/state.json"
        # Freeze only owned processes while editing a snapshot, then continue.
        state = json.loads(path.read_text())
        supervisor_pid = int((self.runtime / state["holders"][0]["run"]).read_text())
        os.kill(supervisor_pid, signal.SIGSTOP)
        try:
            state = json.loads(path.read_text())
            h = state["holders"][0]
            self.assertIsInstance(h["admitted_at"], (float,int))
            self.assertLess(abs(h["admitted_at"] - time.clock_gettime(time.CLOCK_BOOTTIME)), 2)
            for key in ["stopping_at", "warned_at", "warned_for", "warned_waiters", "idle_since", "cpu_usage_usec", "cpu_sample_at", "lease_expires_at"]:
                h.pop(key, None)
            pending = path.with_suffix(".upgrade")
            pending.write_text(json.dumps(state))
            pending.replace(path)
            view = self.status()["slots"][0]
            self.assertTrue(view["admitted_at"].endswith("Z"))
        finally:
            os.kill(supervisor_pid, signal.SIGCONT)
        self.pause(0.3)
        self.assertIsNone(holder.poll())

    def test_memory_ungated_pool_ignores_recovery_and_missing_psi(self):
        self.set_stats(30000)
        holder, output = self.start("holder")
        self.admitted("holder", output)
        self.set_stats(1000)
        self.wait(lambda: holder.poll() is not None)
        self.assertEqual(holder.returncode, 75)
        self.cfg["memory_gated"] = False
        self.cfg["evictable"] = False
        self.write_config()
        (self.proc / "pressure/memory").unlink()
        _, second = self.start("ungated")
        self.wait(second.exists)

    def test_impossible_cost_rejected_before_queue(self):
        result = self.invoke("run", "--cost", "65536", "--", "true")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("MemTotal", result.stderr)
        self.assertFalse((self.runtime / "slotr").exists())

    def test_ipv6_busy_port_is_not_admitted(self):
        try:
            occupied = socket.socket(socket.AF_INET6)
            occupied.bind(("::1", self.cfg["ports"]["base"]))
        except OSError as error:
            self.skipTest(str(error))
        with occupied:
            _, output = self.start("ipv6")
            h = self.admitted("ipv6", output)
            self.assertEqual(h["slot"], 1)

    def test_zero_term_grace_rejected(self):
        self.cfg["term_grace_seconds"] = 0
        self.write_config()
        result = self.invoke("config", "check")
        self.assertEqual(result.returncode, 2)
        self.assertIn("watchdog.term_grace_seconds", result.stderr)

    def test_background_hook_pipe_writer_does_not_delay_supervisor(self):
        self.cfg["on_admit"] = [sys.executable, "-c", "import subprocess,sys; subprocess.Popen([sys.executable,'-c','import time; time.sleep(2)'])"]
        self.write_config()
        self.env["SLOTR_WATCHDOG__ON_PRESSURE"] = "stop"
        holder, output = self.start("holder")
        self.admitted("holder", output)
        self.set_stats(1000)
        self.wait(lambda: holder.poll() is not None, timeout=1.5)
        self.assertEqual(holder.returncode, 75)
        # Wait for the short-lived owned hook descendant before removing scratch dirs.
        self.pause(2)


    def test_warn_claim_cleared_when_holder_exits_without_a_stop(self):
        self.configure_clock()
        self.cfg["grace"] = 0.7
        self.write_config()
        self.set_stats(40000)
        first, p1 = self.start("first", lease="1s")
        self.wait(p1.exists)
        second, p2 = self.start("second", lease="1s")
        self.wait(p2.exists)
        _, waiter = self.start("waiter")
        self.wait(lambda: bool(self.status()["queue"]))
        self.advance(10)
        self.wait(lambda: bool(self.events("warn")))
        self.set_stats(19000)
        first.terminate()
        first.wait(timeout=3)
        self.wait(lambda: second.poll() is not None)
        self.assertEqual(second.returncode, 75)
        self.wait(waiter.exists)
        self.assertEqual(len(self.events("stop")), 1)
        self.assertTrue(self.events("stop")[0]["run"].endswith("-2"))

    def test_unreadable_live_ticket_is_retained(self):
        self.set_stats(10000)
        waiter, _ = self.start("waiter")
        q = self.wait(lambda: self.status()["queue"])
        ticket = self.runtime / "slotr" / ("ticket-" + str(q[0]["enqueue_seq"]))
        ticket.chmod(0)
        try:
            self.assertEqual(len(self.status()["queue"]), 1)
            self.assertIsNone(waiter.poll())
        finally:
            ticket.chmod(0o600)

    def test_ticket_creation_does_not_block_under_state_lock(self):
        root = self.runtime / "slotr"
        root.mkdir()
        with (root / "ticket-1").open("w") as ticket:
            fcntl.flock(ticket, fcntl.LOCK_EX)
            result = self.invoke("run", "--kind", "dev-stack", "--", "true")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads((root / "state.json").read_text())["enqueue_seq"], 2)
        with (root / "state.lock").open("r") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)

    def test_legacy_wall_timestamp_state_remains_readable(self):
        holder, output = self.start("holder")
        h = self.admitted("holder", output)
        supervisor = int((self.runtime / h["run"]).read_text())
        os.kill(supervisor, signal.SIGSTOP)
        try:
            path = self.runtime / "slotr/state.json"
            state = json.loads(path.read_text())
            for key in ["since", "admitted_at", "lease_expires_at"]:
                state["holders"][0][key] = h[key]
            pending = path.with_suffix(".legacy")
            pending.write_text(json.dumps(state))
            pending.replace(path)
            self.assertEqual(self.status()["slots"][0]["state"], "running")
        finally:
            os.kill(supervisor, signal.SIGCONT)
        self.pause(0.3)
        self.assertIsNone(holder.poll())

    def test_release_version_must_match_package(self):
        # Only the read-only git preflight is stubbed. A mismatch exits before
        # cargo or gh can run; no tags, commits or releases are created.
        git = self.bin / "git"
        git.write_text("#!/bin/sh\ncase \"$1:$2\" in status:--porcelain) exit 0;; rev-parse:--verify) exit 1;; *) exit 9;; esac\n")
        git.chmod(0o755)
        result = subprocess.run(["sh", "scripts/release.sh", "9999.0.0"], env=self.env, capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("VERSION must match Cargo.toml", result.stderr)



    def test_probe_r01_a_stop_is_preceded_by_its_own_warning(self):
        self.configure_clock()
        self.cfg['on_warn'] = ['taskr', 'note', '{task}', 'WARN', '{run}', '{reason}']
        self.cfg['grace'] = 1.0
        self.write_config()
        self.set_stats(40000)
        old, p1 = self.start('old', lease='1s')
        self.wait(p1.exists)
        newer, p2 = self.start('newer')
        self.wait(p2.exists)
        self.set_stats(10000)                         # the waiter would not fit even without `old`
        _, waiter = self.start('waiter')
        self.wait(lambda: bool(self.status()['queue']))
        self.advance(10)
        self.wait(lambda: bool(self.events('warn')))
        self.pause(0.5)
        self.set_stats(40000)                         # now releasing `old` admits the waiter
        self.wait(lambda: old.poll() is not None, timeout=8)
        reasons = [e['reason'] for e in self.events('warn')]
        hooks = [a[-1] for a in self.notes() if 'WARN' in a]
        self.assertEqual(old.returncode, 75)
        self.assertIn('lease_expired', reasons, 'stopped after only these warnings: events %s, hook runs %s' % (reasons, hooks))

    def test_probe_r12_a_stop_after_a_cancelled_warning_is_warned_again(self):
        self.configure_clock()
        self.cfg['grace'] = 20
        self.write_config()
        self.set_stats(40000)
        old, p1 = self.start('old', lease='1s')
        self.wait(p1.exists)
        newer, p2 = self.start('newer')
        self.wait(p2.exists)
        self.start('waiter')
        self.wait(lambda: bool(self.status()['queue']))
        self.advance(10)
        self.wait(lambda: bool(self.events('warn')))
        self.set_stats(10000)                         # at grace end the stop would not admit the waiter
        self.advance(25)
        self.wait(lambda: bool(self.events('warn_cancelled')))
        self.set_stats(40000)                         # the holder claims the same ticket again
        self.wait(lambda: any(h.get('state') == 'warned' for h in self.status()['slots']))
        self.advance(25)
        self.wait(lambda: old.poll() is not None)
        log = [json.loads(l) for l in (self.root / 'state/slotr/events.jsonl').read_text().splitlines()]
        order = [e['event'] for e in log if e['event'] in ('warn', 'warn_cancelled', 'stop')]
        self.assertEqual(old.returncode, 75)
        self.assertEqual(order, ['warn', 'warn_cancelled', 'warn', 'stop'], 'event order %s' % order)

    def test_probe_r02_waiter_whose_ticket_left_state_does_not_wait_for_ever(self):
        self.set_stats(10000)
        waiter, _ = self.start('waiter')
        self.wait(lambda: bool(self.status()['queue']))
        (self.runtime / 'slotr/state.json').unlink()  # the operator's only repair for "corrupt state"
        self.pause(1.5)
        queued = len(self.status()['queue'])
        self.assertTrue(waiter.poll() is not None or queued == 1,
                        'waiter alive, not in the queue; log: %s' % (self.root / 'waiter.log').read_text().strip().splitlines()[-2:])

    def test_probe_r03_new_request_can_enqueue_after_state_reset(self):
        self.set_stats(10000)
        self.start('waiter')
        self.wait(lambda: bool(self.status()['queue']))
        (self.runtime / 'slotr/state.json').unlink()
        self.pause(0.5)
        self.set_stats(40000)
        second, out = self.start('second')
        try:
            self.wait(out.exists, timeout=3)
        except AssertionError:
            self.fail('second request rc=%s log=%s' % (second.poll(), (self.root / 'second.log').read_text().strip().splitlines()[-1:]))

    def test_probe_r04_running_holder_stays_accounted_after_state_reset(self):
        holder, p1 = self.start('holder')
        h = self.admitted('holder', p1)
        (self.runtime / 'slotr/state.json').unlink()
        self.pause(1.0)
        self.assertIsNone(holder.poll())
        self.assertIn(h['run'], [s.get('run') for s in self.status()['slots']], 'the workload runs but no holder record exists')

    def test_probe_r05_persistent_tick_error_is_reported_once(self):
        holder, p1 = self.start('holder')
        self.admitted('holder', p1)
        (self.runtime / 'ctl-fail').touch()
        self.pause(2.0)
        (self.runtime / 'ctl-fail').unlink()
        events = len(self.events('supervisor_tick_error'))
        lines = (self.root / 'holder.log').read_text().count('supervisor tick')
        self.assertIsNone(holder.poll())
        self.assertLessEqual(max(events, lines), 2, '%d events and %d stderr lines in 2 s' % (events, lines))

    def test_probe_r06_emergency_stop_still_happens_while_the_manager_fails(self):
        self.set_stats(30000)
        holder, p1 = self.start('holder')
        self.admitted('holder', p1)
        (self.runtime / 'ctl-fail').touch()
        self.set_stats(1000)                          # below emergency_available_mib
        self.pause(2.0)
        rc = holder.poll()
        (self.runtime / 'ctl-fail').unlink()
        self.assertEqual(rc, 75, 'no stop in 2 s (28 ticks) at 1000 MiB available; stop events %d' % len(self.events('stop')))

    def test_probe_r07_a_failed_workload_is_not_run_twice(self):
        self.set_stats(60000)
        _, p0 = self.start('other')                   # a second holder: its supervisor reconciles state
        self.admitted('other', p0)
        marker = self.root / 'ran'
        code = ("import subprocess, sys; open(%r, 'a').write('ran\\n'); "
                "print('error: object already exists', file=sys.stderr, flush=True); "
                "subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(1.2)']); sys.exit(1)") % str(marker)
        child = subprocess.Popen([str(TOOL), 'run', '--pool', 'runtime', '--kind', 'dev-stack', '--campaign', 'x', '--purpose', 'p',
                                  '--', sys.executable, '-c', code], env=self.env, stdin=subprocess.DEVNULL,
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.children.append(child)
        try:
            child.wait(timeout=6)
        except subprocess.TimeoutExpired:
            pass
        runs = marker.read_text().count('ran')
        self.assertEqual((runs, child.poll()), (1, 1), 'the command ran %d times; frontend rc=%s' % (runs, child.poll()))

    def test_probe_r08_status_survives_a_huge_lease(self):
        self.cfg['max_lease'] = '0'
        self.write_config()
        _, p1 = self.start('big', lease='1e15')
        self.wait(p1.exists)
        result = self.invoke('status', '--json')
        self.assertEqual(result.returncode, 0, result.stderr.strip()[:160])

    def test_probe_r09_waiter_frozen_during_grace_cancels_at_grace_end(self):
        self.configure_clock()
        self.cfg['grace'] = 20
        self.write_config()
        self.set_stats(40000)
        old, p1 = self.start('old', lease='1s')
        self.wait(p1.exists)
        newer, p2 = self.start('newer')
        self.wait(p2.exists)
        waiter, out = self.start('waiter')
        self.wait(lambda: bool(self.status()['queue']))
        self.advance(10)
        self.wait(lambda: any(h.get('state') == 'warned' for h in self.status()['slots']))
        os.kill(waiter.pid, signal.SIGSTOP)
        try:
            self.advance(30)
            self.pause(2.0)
            self.assertIsNone(old.poll(), 'stopped rc=%s for a waiter frozen during the grace' % old.returncode)
            self.assertEqual(len(self.events('warn_cancelled')), 1)
            self.assertEqual(self.events('stop'), [])
        finally:
            os.kill(waiter.pid, signal.SIGCONT)

    def test_probe_r10_stop_of_a_term_ignoring_workload(self):
        self.cfg['term_grace_seconds'] = 2
        self.write_config()
        holder, p1 = self.start('holder', '--ignore-term')
        h = self.admitted('holder', p1)
        began = time.monotonic()
        result = subprocess.run([str(TOOL), 'stop', h['run']], env=self.env, capture_output=True, text=True, timeout=20)
        took = time.monotonic() - began
        holder.wait(timeout=10)
        total = time.monotonic() - began
        self.assertEqual(result.returncode, 0, result.stderr.strip())
        self.assertLess(took, 1.0)
        self.assertEqual(holder.returncode, 143)
        self.assertTrue(1.5 < total < 4.5, 'unit ended after %.1f s (grace 2)' % total)

    def test_probe_r11_warned_holder_stops_after_grace_and_waiter_is_admitted(self):
        self.configure_clock()
        self.cfg['grace'] = 20
        self.cfg['on_warn'] = ['taskr', 'note', '{task}', 'WARN', '{run}', '{reason}']
        self.write_config()
        self.set_stats(40000)
        old, p1 = self.start('old', lease='1s')
        self.wait(p1.exists)
        newer, p2 = self.start('newer')
        self.wait(p2.exists)
        _, out = self.start('waiter')
        self.wait(lambda: bool(self.status()['queue']))
        self.advance(10)
        self.wait(lambda: bool(self.events('warn')))
        self.pause(0.6)
        self.assertIsNone(old.poll(), 'stopped before the grace ended')
        self.advance(25)
        self.wait(lambda: old.poll() is not None)
        self.wait(out.exists)
        self.assertEqual(old.returncode, 75)
        self.assertEqual([e['reason'] for e in self.events('warn')], ['lease_expired'])
        self.assertEqual(len(self.events('stop')), 1)
        self.assertIsNone(newer.poll())

    def test_probe_r13_one_pressure_stop_while_the_newest_holder_is_stopping(self):
        self.cfg['term_grace_seconds'] = 1.5
        self.cfg['recovery_healthy_seconds'] = 5.0
        self.write_config()
        self.set_stats(40000)
        older, p1 = self.start('older')
        h1 = self.admitted('older', p1)
        newest, p2 = self.start('newest', '--ignore-term')
        h2 = self.admitted('newest', p2)
        self.set_stats(1000)                          # emergency
        self.wait(lambda: bool(self.events('stop')))
        self.pause(0.7)
        states = {s['run']: s['state'] for s in self.status()['slots']}
        self.assertEqual(states.get(h2['run']), 'stopping')
        self.assertIn(states.get(h1['run']), ('running', 'overdue'))
        self.wait(lambda: newest.poll() is not None)
        self.pause(0.5)
        self.assertEqual(newest.returncode, 75)
        self.assertIsNone(older.poll())
        self.assertEqual([e['run'] for e in self.events('stop')], [h2['run']])

    def test_probe_r14_status_times_are_wall_clock_and_lease_length_holds(self):
        import datetime
        _, p1 = self.start('holder', lease='90s')
        h = self.admitted('holder', p1)
        parse = lambda v: datetime.datetime.fromisoformat(v.replace('Z', '+00:00')).timestamp()
        self.assertLess(abs(parse(h['admitted_at']) - time.time()), 10)
        self.assertAlmostEqual(parse(h['lease_expires_at']) - parse(h['admitted_at']), 90, delta=0.01)
        raw = json.loads((self.runtime / 'slotr/state.json').read_text())['holders'][0]
        self.assertLess(abs(raw['admitted_at'] - time.clock_gettime(time.CLOCK_BOOTTIME)), 10)
        self.assertLess(abs(parse(self.events('admit')[0]['at']) - time.time()), 10)

    def test_probe_r15_non_utf8_value_is_passed_by_name_only(self):
        env = {os.fsencode(k): os.fsencode(v) for k, v in self.env.items()}
        env[b'LEGACY_LATIN1'] = b'caf\xe9'
        out = self.root / 'latin'
        child = subprocess.Popen([str(TOOL), 'run', '--pool', 'runtime', '--kind', 'dev-stack', '--campaign', 'x', '--purpose', 'p', '--',
                                  sys.executable, '-c', WORKLOAD, str(out)], env=env, stdin=subprocess.DEVNULL,
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.children.append(child); self.workloads.append(out)
        self.wait(out.exists)
        flags = json.loads((self.runtime / 'launches').read_text().splitlines()[0])
        self.assertIn('--setenv=LEGACY_LATIN1', flags)   # documents the behaviour: real systemd-run must cope with the value
        self.assertFalse(any('caf' in f for f in flags))


    def test_claimant_keeps_waiter_when_an_older_holder_becomes_eligible(self):
        self.configure_clock()
        self.cfg['grace'] = 100
        self.write_config()
        self.set_stats(40000)
        older, p1 = self.start('older', campaign='A', lease='60s')
        self.wait(p1.exists)
        newer, p2 = self.start('newer', campaign='A')
        self.wait(p2.exists)
        _, waiter = self.start('waiter', campaign='B')
        self.wait(lambda: bool(self.events('warn')))
        claimed_run = self.events('warn')[0]['run']
        self.assertEqual(self.events('warn')[0]['reason'], 'campaign_yield')
        self.advance(110)
        self.wait(lambda: newer.poll() is not None)
        self.wait(waiter.exists)
        self.assertEqual(newer.returncode, 75)
        self.assertIsNone(older.poll())
        self.assertEqual([e['run'] for e in self.events('warn')], [claimed_run])
        self.assertEqual([e['run'] for e in self.events('stop')], [claimed_run])
        self.assertEqual(self.events('warn_cancelled'), [])

if __name__ == '__main__':
    unittest.main(verbosity=2, failfast=True)
