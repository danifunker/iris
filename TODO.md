DONE net - ftp hangs at 48K, examine tcp window handling

rex
skip first/last for blocks?

line stipple

DONE why is inactive terminal caret slanted and login picture frame too, bresenham innacuracy or something else?

logicop=src fastpath

fract lines

aa lines

DONE dithering

HALFASSED cursor hotspot alignment

DONEish faster/better/lower latency gfifo

DONE octahedra not rendering the bottom of teh screen

cpu
DONE add callback on status writes so we can add side effects of user<>kernel 64<>32 switch and such

DONE nanotlb, cache last successfull address full translation for 3 access types.
reset on kernel<>user switch and tlb ops

DONE split translate into translate_32/64_kernel/user add function pointer

DONE new self-calibrating fasttick

DONEish fpu
look at ide fpu test, convert to user space test, compile and run on irix, fix failures

watchhi/watchlo register support for debugging, translate variants that fire exception when hit


vino — basic pixel pipeline, CDMC stub, and macOS UVC camera capture are
       in (see [vino] in iris.toml).  Remaining work:
       - per-port routing via SELECT_D1: today both VINO channels see the
         same source; the real chip selects between SAA7191 composite (D0)
         and CDMC IndyCam (D1) per channel
       - I2C repeated-start so IRIX drivers that skip the subaddr resend
         for reads (the standard protocol) work without a workaround
       - end-to-end visual verification under IRIX (needs vl_eoe /
         vino_eoe / indycam_eoe installed)

DONE scsi - eject, load cd

DONE scsi - C9 command

scsi - cdrom no sense in irix 5.3

DONE scsi - large requests fail

ui - file selection for cd load

DONE bus - writemask on write64 and write32 for more efficient uncached store left/right?

all - examine locks, possibly switch to spin, first check peripherials then lock in hpc and ioc


IP7 interrupt weirdness after reset/reboot

Xsgi crash in irix 5.3 R5K

DONE? early rex3 revision?



