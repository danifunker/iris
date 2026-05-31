/*
 * vinograb.c — grab one frame from the IndyCam (VINO) into a file.
 *
 * Uses the IRIX Video Library (VL): open the VINO server, build a
 * path from the digital video source (the IndyCam) to a memory drain,
 * capture a single RGB frame, and write the raw pixels to a file.
 *
 *   cc -o vinograb vinograb.c -lvl
 *   /usr/etc/videod &           # the VL daemon must be running
 *   ./vinograb /tmp/grab.rgb
 *
 * Output is raw bytes: tsize = xsize*ysize*3 for VL_PACKING_RGB_8,
 * top-to-bottom, R,G,B per pixel. Dimensions are printed to stderr.
 */
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <vl/vl.h>

static void die(const char *msg)
{
    fprintf(stderr, "%s: %s\n", msg, vlStrError(vlGetErrno()));
    exit(1);
}

int main(int argc, char **argv)
{
    VLServer svr;
    VLPath   path;
    VLNode   src, drn;
    VLControlValue val;
    VLBuffer buf;
    VLInfoPtr info;
    void *dataPtr;
    int xsize, ysize, tsize, tries, bpp;
    const char *outfile = (argc > 1) ? argv[1] : "/tmp/grab.rgb";
    FILE *f;

    svr = vlOpenVideo("");
    if (svr == NULL) { fprintf(stderr, "vlOpenVideo failed\n"); exit(1); }

    /* Let VL pick the default video source and a free memory drain. */
    src = vlGetNode(svr, VL_SRC, VL_VIDEO, VL_ANY);
    if (src < 0) die("vlGetNode src");
    drn = vlGetNode(svr, VL_DRN, VL_MEM, VL_ANY);
    if (drn < 0) die("vlGetNode drn");

    path = vlCreatePath(svr, VL_ANY, src, drn);
    if (path < 0) die("vlCreatePath");
    if (vlSetupPaths(svr, (VLPathList)&path, 1, VL_SHARE, VL_SHARE) < 0)
        die("vlSetupPaths");

    /* Ask for 24-bit RGB; fall back to 32-bit RGBA if the drain refuses. */
    bpp = 3;
    val.intVal = VL_PACKING_RGB_8;
    if (vlSetControl(svr, path, drn, VL_PACKING, &val) < 0) {
        val.intVal = VL_PACKING_RGBA_8;
        if (vlSetControl(svr, path, drn, VL_PACKING, &val) < 0)
            die("vlSetControl packing");
        bpp = 4;
    }

    /* Interleave both fields into one full frame (best effort). */
    val.intVal = VL_CAPTURE_INTERLEAVED;
    vlSetControl(svr, path, drn, VL_CAP_TYPE, &val);

    if (vlGetControl(svr, path, drn, VL_SIZE, &val) < 0) die("vlGetControl size");
    xsize = val.xyVal.x;
    ysize = val.xyVal.y;

    buf = vlCreateBuffer(svr, path, drn, 3);
    if (buf == NULL) die("vlCreateBuffer");
    if (vlRegisterBuffer(svr, path, drn, buf) < 0) die("vlRegisterBuffer");

    tsize = vlGetTransferSize(svr, path);
    fprintf(stderr, "vinograb: %dx%d, %d bpp, transfer=%d bytes\n",
            xsize, ysize, bpp, tsize);

    /* Select transfer-complete events and drive a vlNextEvent loop — the VL
       library advances the buffer ring as it processes events (this is what
       vidtomem does; polling vlGetNextValid without the event pump never sees
       a valid frame). */
    vlSelectEvents(svr, path, VLTransferCompleteMask);
    if (vlBeginTransfer(svr, path, 0, NULL) < 0) die("vlBeginTransfer");

    info = NULL;
    tries = 0;
    while ((info = vlGetNextValid(svr, buf)) == NULL
        && (info = vlGetLatestValid(svr, buf)) == NULL) {
        if (++tries > 4000) { fprintf(stderr, "timeout (errno %d)\n", vlGetErrno()); exit(2); }
        sginap(1);
    }

    dataPtr = vlGetActiveRegion(svr, buf, info);
    f = fopen(outfile, "w");
    if (!f) { perror("fopen"); exit(1); }
    fwrite(dataPtr, 1, tsize, f);
    fclose(f);
    fprintf(stderr, "vinograb: wrote %d bytes to %s\n", tsize, outfile);

    vlPutFree(svr, buf);
    vlEndTransfer(svr, path);
    vlDestroyPath(svr, path);
    vlCloseVideo(svr);
    return 0;
}
