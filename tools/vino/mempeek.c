/* mempeek <hexphysaddr> [nwords] — dump physical memory via /dev/mem */
#include <stdio.h>
#include <stdlib.h>
#include <fcntl.h>
#include <unistd.h>

int main(int argc, char **argv)
{
    unsigned long addr, n, i;
    int fd;
    unsigned int w;

    if (argc < 2) { fprintf(stderr, "usage: mempeek <hexaddr> [nwords]\n"); return 1; }
    addr = strtoul(argv[1], 0, 16);
    n    = (argc > 2) ? strtoul(argv[2], 0, 10) : 8;

    fd = open("/dev/mem", O_RDONLY);
    if (fd < 0) { perror("open /dev/mem"); return 1; }

    for (i = 0; i < n; i++) {
        unsigned long a = addr + i * 4;
        if (lseek(fd, (off_t)a, SEEK_SET) == (off_t)-1) { perror("lseek"); return 1; }
        if (read(fd, &w, 4) != 4) { perror("read"); return 1; }
        printf("%08lx: %08x\n", a, w);
    }
    close(fd);
    return 0;
}
