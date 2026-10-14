#define _POSIX_C_SOURCE 200809L
#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef SLOTR_BINARY
#error Define SLOTR_BINARY as a literal absolute path
#endif
#ifndef SLOTR_CONFIG
#error Define SLOTR_CONFIG as a literal absolute path
#endif

int main(int argc, char **argv)
{
    if (SLOTR_BINARY[0] != '/' || SLOTR_CONFIG[0] != '/' || argc < 1 ||
        (size_t)argc > SIZE_MAX / sizeof(char *) - 5) {
        fputs("flock guard: invalid fixed paths or argument count\n", stderr);
        return 2;
    }
    char **check = malloc(((size_t)argc + 5) * sizeof(*check));
    if (!check) {
        perror("flock guard: checker arguments");
        return 2;
    }
    check[0] = SLOTR_BINARY;
    check[1] = "flock-check";
    check[2] = "--config";
    check[3] = SLOTR_CONFIG;
    check[4] = "--";
    for (int i = 1; i < argc; i++)
        check[(size_t)i + 4] = argv[i];
    check[(size_t)argc + 4] = NULL;

    struct sigaction original, temporary = {0};
    if (sigaction(SIGCHLD, NULL, &original) < 0) {
        perror("flock guard: read SIGCHLD action");
        free(check);
        return 2;
    }
    int changed = original.sa_handler == SIG_IGN || (original.sa_flags & SA_NOCLDWAIT);
    temporary.sa_handler = SIG_DFL;
    if (sigemptyset(&temporary.sa_mask) < 0 ||
        (changed && sigaction(SIGCHLD, &temporary, NULL) < 0)) {
        perror("flock guard: prepare SIGCHLD action");
        free(check);
        return 2;
    }
    pid_t child = fork();
    if (child == 0) {
        execv(SLOTR_BINARY, check);
        perror("flock guard: exec checker");
        _exit(127);
    }
    free(check);
    int code = 2, status;
    if (child < 0) {
        perror("flock guard: fork checker");
    } else {
        pid_t waited;
        do {
            waited = waitpid(child, &status, WUNTRACED);
        } while (waited < 0 && errno == EINTR);
        if (waited != child) {
            fputs("flock guard: could not wait for checker\n", stderr);
        } else if (WIFEXITED(status)) {
            code = WEXITSTATUS(status);
        } else if (WIFSTOPPED(status)) {
            /* This PID is still our unreaped child, never an arbitrary caller PID. */
            if (kill(child, SIGKILL) < 0) {
                perror("flock guard: kill stopped checker");
            } else {
                do {
                    waited = waitpid(child, &status, 0);
                } while (waited < 0 && errno == EINTR);
                if (waited != child)
                    fputs("flock guard: could not reap stopped checker\n", stderr);
            }
            fputs("flock guard: checker stopped\n", stderr);
        } else if (WIFSIGNALED(status)) {
            code = 128 + WTERMSIG(status);
            fputs("flock guard: checker signaled\n", stderr);
        } else {
            fputs("flock guard: unknown checker status\n", stderr);
        }
    }
    if (changed && sigaction(SIGCHLD, &original, NULL) < 0) {
        perror("flock guard: restore SIGCHLD action");
        return 2;
    }
    if (code != 0)
        return code;
    argv[0] = "/usr/bin/flock";
    execv(argv[0], argv);
    perror("flock guard: exec native flock");
    return 127;
}
