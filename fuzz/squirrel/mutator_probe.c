#include <dlfcn.h>
#include <stdio.h>
#include <string.h>

typedef void *(*init_fn)(void *, unsigned int);
typedef unsigned int (*count_fn)(void *, const unsigned char *, size_t);
typedef size_t (*fuzz_fn)(void *, unsigned char *, size_t, unsigned char **,
                          unsigned char *, size_t, size_t);
typedef unsigned char (*queue_fn)(void *, const unsigned char *,
                                  const unsigned char *);

int main(int argc, char **argv) {
  if (argc < 2) { fprintf(stderr, "usage: probe <seed.sql>\n"); return 2; }
  void *h = dlopen("/opt/squirrel-src/build/libpostgresql_mutator.so", RTLD_NOW);
  if (!h) { fprintf(stderr, "dlopen: %s\n", dlerror()); return 1; }
  init_fn init = (init_fn)dlsym(h, "afl_custom_init");
  count_fn count = (count_fn)dlsym(h, "afl_custom_fuzz_count");
  fuzz_fn fuzz = (fuzz_fn)dlsym(h, "afl_custom_fuzz");
  queue_fn queue_new = (queue_fn)dlsym(h, "afl_custom_queue_new_entry");
  void *m = init(NULL, 1337);
  fprintf(stderr, "== init done\n");
  queue_new(m, (const unsigned char *)argv[1], NULL);
  fprintf(stderr, "== queue_new_entry done\n");

  FILE *f = fopen(argv[1], "rb");
  char buf[8192];
  size_t len = fread(buf, 1, sizeof(buf), f);
  fclose(f);

  unsigned int n = count(m, (const unsigned char *)buf, len);
  fprintf(stderr, "== fuzz_count: %u\n", n);
  for (unsigned int i = 0; i < n && i < 5; i++) {
    unsigned char *out = NULL;
    size_t sz = fuzz(m, (unsigned char *)buf, len, &out, NULL, 0, 1 << 20);
    printf("--- mutation %u (%zu bytes):\n%.*s\n", i, sz, (int)sz, out);
  }
  return 0;
}
