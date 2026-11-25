// SPDX-License-Identifier: MIT
/* Delay only the selected observer config read or atomic state fsync. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <sys/syscall.h>

static int matches_path(int fd, const char *needle) {
  char path[PATH_MAX];
#if defined(__APPLE__)
  if (fcntl(fd, F_GETPATH, path) != 0) return 0;
#else
  char link[64];
  snprintf(link, sizeof(link), "/proc/self/fd/%d", fd);
  ssize_t length = readlink(link, path, sizeof(path) - 1);
  if (length < 0) return 0;
  path[length] = '\0';
#endif
  return strstr(path, needle) != NULL;
}

static void delay_once(volatile int *already_delayed) {
  if (__sync_lock_test_and_set(already_delayed, 1)) return;
  struct timespec delay = {.tv_sec = 2, .tv_nsec = 0};
  nanosleep(&delay, NULL);
}

static ssize_t delayed_read(int fd, void *buffer, size_t length) {
  static ssize_t (*real_read)(int, void *, size_t);
  static volatile int already_delayed;
  static __thread int resolving;
  if (!real_read) {
    if (resolving) return syscall(SYS_read, fd, buffer, length);
    resolving = 1;
    real_read = dlsym(RTLD_NEXT, "read");
    resolving = 0;
  }
  const char *mode = getenv("LOCKED_BUILD_DELAY_IO");
  if (mode && strcmp(mode, "config-read") == 0 &&
      matches_path(fd, "policy.json")) {
    delay_once(&already_delayed);
  }
  return real_read(fd, buffer, length);
}

static int delayed_fsync(int fd) {
  static int (*real_fsync)(int);
  static volatile int already_delayed;
  static __thread int resolving;
  if (!real_fsync) {
    if (resolving) return syscall(SYS_fsync, fd);
    resolving = 1;
    real_fsync = dlsym(RTLD_NEXT, "fsync");
    resolving = 0;
  }
  const char *mode = getenv("LOCKED_BUILD_DELAY_IO");
  if (mode && strcmp(mode, "state-fsync") == 0 &&
      matches_path(fd, "observe.json.next")) {
    delay_once(&already_delayed);
  }
  return real_fsync(fd);
}

#if defined(__APPLE__)
__attribute__((used)) static struct {
  const void *replacement;
  const void *replacee;
} interpose_read __attribute__((section("__DATA,__interpose"))) = {
    (const void *)delayed_read, (const void *)read};
__attribute__((used)) static struct {
  const void *replacement;
  const void *replacee;
} interpose_fsync __attribute__((section("__DATA,__interpose"))) = {
    (const void *)delayed_fsync, (const void *)fsync};
#else
ssize_t read(int fd, void *buffer, size_t length) {
  return delayed_read(fd, buffer, length);
}

int fsync(int fd) { return delayed_fsync(fd); }
#endif
