/* chainwalk <hexstart> — follow the VINO descriptor chain exactly as iris's
 * shift_descriptors does (fetch 4, use slot0/page, shift, JUMP->fetch target),
 * report data pages until STOP and any backward (ring) jump. */
#include <stdio.h>
#include <stdlib.h>
#include <fcntl.h>
#include <unistd.h>
static unsigned int rd(int fd,unsigned long a){unsigned int w=0;lseek(fd,(off_t)a,0);read(fd,&w,4);return w;}
int main(int argc,char**argv){
    unsigned long start; int fd; unsigned int cache[4]; int valid[4];
    unsigned long ndp; int i; long pages=0,jumps=0,back=0,guard=0;
    unsigned long laddr,firststop=0;
    start=(argc>1)?strtoul(argv[1],0,16):0x0861e000ul;
    fd=open("/dev/mem",O_RDONLY); if(fd<0){perror("mem");return 1;}
    ndp=start; laddr=start;
    for(i=0;i<4;i++){cache[i]=rd(fd,ndp+i*4);valid[i]=1;} ndp+=16;
    while(guard++<400000){
        unsigned int d=cache[0]; int v=valid[0];
        if(v && (d&0x80000000u)){firststop=laddr;break;}
        if(v && (d&0x40000000u)){
            unsigned long tgt=(unsigned long)(d&0x3fffffffu);
            jumps++; if(tgt<=laddr) back++;
            laddr=tgt; for(i=0;i<4;i++){cache[i]=rd(fd,tgt+i*4);valid[i]=1;} ndp=tgt+16;
            continue;
        }
        if(v && d) pages++;
        /* shift */
        cache[0]=cache[1];valid[0]=valid[1];
        cache[1]=cache[2];valid[1]=valid[2];
        cache[2]=cache[3];valid[2]=valid[3];
        valid[3]=0;
        if(!valid[0]){for(i=0;i<4;i++){cache[i]=rd(fd,ndp+i*4);valid[i]=1;}laddr=ndp;ndp+=16;}
    }
    printf("start=%08lx pages=%ld jumps=%ld backjumps(ring)=%ld first_stop=%08lx guard=%ld\n",
        start,pages,jumps,back,firststop,guard);
    return 0;
}
