/* chaindump <hexstart> — dump the VINO descriptor chain: for each 32-bit
 * descriptor word show its type (DATA/JUMP/STOP) and address/target, following
 * JUMPs (16-byte aligned, like the hardware) until STOP or a guard limit.
 * Prints the first N and the run near STOP so we can see the interlace layout. */
#include <stdio.h>
#include <stdlib.h>
#include <fcntl.h>
#include <unistd.h>
static unsigned int rd(int fd, unsigned long a){unsigned int w=0;lseek(fd,(off_t)a,0);read(fd,&w,4);return w;}
int main(int argc, char **argv){
    unsigned long cur; int fd; unsigned int cache[4]; int slot;
    unsigned long laddr; long pages=0, jumps=0, guard=0; int printed=0;
    cur=(argc>1)?strtoul(argv[1],0,16):0x0861e000ul;
    fd=open("/dev/mem",O_RDONLY); if(fd<0){perror("mem");return 1;}
    laddr=cur;
    for(slot=0;slot<4;slot++) cache[slot]=rd(fd,cur+slot*4);
    slot=0;
    while(guard++<4000){
        unsigned int d=cache[slot];
        int doprint = (printed<40) || (pages>=170);
        if(d & 0x80000000u){
            if(doprint) printf("[%6ld] @%08lx STOP  word=%08x\n",guard,laddr,d);
            break;
        } else if(d & 0x40000000u){
            unsigned long tgt=(unsigned long)(d & 0x3ffffff0u); /* 16-byte aligned */
            unsigned long tgtu=(unsigned long)(d & 0x3fffffffu);
            jumps++;
            if(doprint){ printf("[%6ld] @%08lx JUMP  ->%08lx (raw lowbits %lx)\n",guard,laddr,tgt,tgtu&0xf); printed++; }
            laddr=tgt;
            for(slot=0;slot<4;slot++) cache[slot]=rd(fd,tgt+slot*4);
            slot=0;
            continue;
        } else {
            pages++;
            if(doprint){ printf("[%6ld] @%08lx DATA  page=%08x\n",guard,laddr,d & 0x3fffffff); printed++; }
        }
        slot++; laddr+=4;
        if(slot==4){ for(slot=0;slot<4;slot++) cache[slot]=rd(fd,laddr+slot*4); slot=0; }
    }
    printf("TOTAL pages=%ld jumps=%ld guard=%ld\n",pages,jumps,guard);
    return 0;
}
