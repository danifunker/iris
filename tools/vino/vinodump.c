/*
 * vinodump.c — reconstruct the captured VINO frame straight from the DMA
 * target pages in physical memory, bypassing the VL client ring.
 *
 * Reads channel A's descriptor-table pointer and line-size from the VINO
 * registers, walks the descriptor list collecting data-page addresses (a
 * page pointer has bits [31:30] clear; JUMP has bit30; STOP has bit31),
 * reads each 4 KB page, and writes them in order to a file. The result is
 * the raw frame buffer: 480 rows of `stride` bytes, each row beginning with
 * 640 ARGB pixels (A,R,G,B).
 *
 *   cc -o vinodump vinodump.c ; ./vinodump /var/tmp/frame.raw
 */
#include <stdio.h>
#include <stdlib.h>
#include <fcntl.h>
#include <unistd.h>

#define VINO_BASE        0x00080000u
#define CHA_DESC_TABLE   (VINO_BASE + 0x0074u) /* CHA CH_DESC_TABLE_PTR low word */
#define CHA_NEXT_DESC    (VINO_BASE + 0x006Cu) /* CHA CH_NEXT_4_DESC low word    */
#define CHA_LINE_SIZE    (VINO_BASE + 0x0054u) /* CHA CH_LINE_SIZE low word      */

#define NPAGES 300   /* 640*480*4 = 1228800 = 300 * 4096 */
#define PAGESZ 4096

static unsigned int rd32(int fd, unsigned long pa)
{
    unsigned int w = 0;
    if (lseek(fd, (off_t)pa, SEEK_SET) == (off_t)-1) { perror("lseek"); exit(1); }
    if (read(fd, &w, 4) != 4) { perror("read"); exit(1); }
    return w;
}

int main(int argc, char **argv)
{
    const char *out = (argc > 1) ? argv[1] : "/var/tmp/frame.raw";
    int fd, ofd, got = 0;
    unsigned long table;
    unsigned long scan;
    static unsigned char page[PAGESZ];
    int i;

    /* /dev/mem can't read the VINO I/O registers, so take the descriptor-table
       physical address as an argument (videod allocates it deterministically at
       0x0861e000 on this config). */
    table = (argc > 2) ? strtoul(argv[2], 0, 16) : 0x0861e000ul;

    fd = open("/dev/mem", O_RDONLY);
    if (fd < 0) { perror("open /dev/mem"); return 1; }
    fprintf(stderr, "vinodump: desc table=%08lx\n", table);

    ofd = open(out, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (ofd < 0) { perror("open out"); return 1; }

    /* Follow the descriptor chain in hardware order: each 16-byte group is
       up to 4 words; a JUMP word (bit30) redirects to its target 16-byte
       aligned (& 0x3ffffff0); a STOP word (bit31) ends it. Aligning the jump
       target is the fix — following it unaligned skips one page per group and
       scrambles the row order. */
    scan = table & 0x3ffffff0u;
    {
        int guard = 0;
        while (got < NPAGES && guard++ < 100000) {
            int slot, jumped = 0;
            for (slot = 0; slot < 4 && got < NPAGES; slot++) {
                unsigned int d = rd32(fd, scan + (unsigned long)slot * 4);
                if (d & 0x80000000u) { guard = 100001; break; }   /* STOP */
                if (d & 0x40000000u) { scan = (unsigned long)(d & 0x3ffffff0u); jumped = 1; break; }
                if (d == 0) continue;
                {
                    unsigned long pg = (unsigned long)(d & 0x3ffff000u);
                    unsigned int off;
                    for (off = 0; off < PAGESZ; off += 4) {
                        unsigned int w = rd32(fd, pg + off);
                        page[off    ] = (w >> 24) & 0xff;
                        page[off + 1] = (w >> 16) & 0xff;
                        page[off + 2] = (w >>  8) & 0xff;
                        page[off + 3] =  w        & 0xff;
                    }
                    if (write(ofd, page, PAGESZ) != PAGESZ) { perror("write"); return 1; }
                    got++;
                }
            }
            if (guard > 100000) break;
            if (!jumped) scan += 16;
        }
    }
    close(ofd);
    close(fd);
    fprintf(stderr, "vinodump: wrote %d pages (%d bytes) to %s\n", got, got * PAGESZ, out);
    return 0;
}
