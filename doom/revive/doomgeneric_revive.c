/*
 * doomgeneric_revive.c
 *
 * doomgeneric platform backend for Revive Phone: a Linux ARM phone
 * (postmarketOS, musl) where Doom is drawn straight to the framebuffer
 * (/dev/fb0) and played with on-screen touch controls.
 *
 * The physical panel is used in portrait (e.g. 720x1280, 24 bpp, B/G/R
 * byte order) and the game is presented in a 1280x720 landscape canvas,
 * rotated on the fly according to /var/lib/revive/rotation.
 *
 * This file is part of Revive Phone and is distributed under the terms of
 * the GNU General Public License version 2.  See ../LICENSE.
 *
 * It uses nothing but libc and Linux headers: no SDL, no X11, no sound.
 */

#define _GNU_SOURCE

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <linux/fb.h>
#include <linux/input.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

#include "doomgeneric.h"

/* doomkeys.h re-defines a handful of names that also exist in
 * <linux/input.h>, with completely different values (Doom key codes vs
 * evdev codes).  Drop the evdev ones we never use, and keep the two we do
 * need under our own names. */
enum {
    EVDEV_ENTER = KEY_ENTER, /* 28 */
    EVDEV_TAB = KEY_TAB,     /* 15 */
};

#undef KEY_ENTER
#undef KEY_TAB
#undef KEY_MINUS
#undef KEY_EQUALS
#undef KEY_BACKSPACE
#undef KEY_PAUSE
#undef KEY_CAPSLOCK
#undef KEY_NUMLOCK
#undef KEY_HOME
#undef KEY_END
#undef KEY_PGUP
#undef KEY_PGDN
#undef KEY_INS
#undef KEY_DEL
#undef KEY_F1
#undef KEY_F2
#undef KEY_F3
#undef KEY_F4
#undef KEY_F5
#undef KEY_F6
#undef KEY_F7
#undef KEY_F8
#undef KEY_F9
#undef KEY_F10
#undef KEY_F11
#undef KEY_F12

#include "doomkeys.h"

/* ------------------------------------------------------------------ */
/* Layout constants (landscape, in "virtual" 1280x720 coordinates)     */
/* ------------------------------------------------------------------ */

#define LAND_W 1280
#define LAND_H 720

#define DPAD_CX 180
#define DPAD_CY 540
#define DPAD_R  150

#define FIRE_CX 1110
#define FIRE_CY 560
#define FIRE_R  110

#define USE_CX 940
#define USE_CY 640
#define USE_R  70

#define WEAPON_CX 1180
#define WEAPON_CY 380
#define WEAPON_R  60

#define ESC_X0   10
#define ESC_Y0   10
#define ESC_X1  150
#define ESC_Y1   80

#define MAP_X0 1130
#define MAP_Y0   10
#define MAP_X1 1270
#define MAP_Y1   80

#define ENTER_X0 990
#define ENTER_Y0  10
#define ENTER_X1 1120
#define ENTER_Y1  80

/* 35% white overlay, in 8.8 fixed point */
#define OVERLAY_ALPHA 89
#define OVERLAY_BLACK ((255 * OVERLAY_ALPHA) >> 8)

#define DEADZONE2 (30 * 30)

/* ------------------------------------------------------------------ */
/* Framebuffer state                                                   */
/* ------------------------------------------------------------------ */

static int g_fb_fd = -1;
static uint8_t *g_fb_mem = NULL;
static size_t g_fb_len = 0;
static struct fb_var_screeninfo g_vi;
static struct fb_fix_screeninfo g_fi;

static int g_bpp = 32;          /* 24 or 32 */
static int g_line_len = 0;
static int g_pw = 0, g_ph = 0;  /* physical (portrait) resolution */
static int g_rot = 270;         /* 90, 270 or 0 (already landscape) */
static int g_lw = 0, g_lh = 0;  /* logical landscape size */
static uint32_t g_alpha = 0;    /* alpha bits to OR into 32bpp pixels */

static uint32_t *g_land = NULL; /* landscape canvas */
static uint8_t *g_mask = NULL;  /* overlay mask, 1 = blend 35% white */

static int g_dw = 0, g_dh = 0;  /* scaled doom rect inside the canvas */
static int g_dx0 = 0, g_dy0 = 0;
static int *g_colmap = NULL;
static int *g_rowmap = NULL;

/* ------------------------------------------------------------------ */
/* Input state                                                         */
/* ------------------------------------------------------------------ */

#define MAX_DEV   32
#define MAX_SLOTS 16
#define QUEUE_LEN 64

struct touch_slot {
    int active;
    int x, y;       /* raw evdev coordinates */
    int weapon;     /* was resting on the weapon button */
};

struct input_dev {
    int fd;
    int touch;      /* touchscreen */
    int has_slot;   /* protocol B seen */
    int btn_touch;  /* BTN_TOUCH state (protocol A) */
    int cur_slot;
    int dirty;
    int hat[2];
    struct touch_slot slots[MAX_SLOTS];
};

static struct input_dev g_dev[MAX_DEV];
static int g_ndev = 0;

/* Doom keys that the touch overlay can hold down */
static const unsigned char g_managed[] = {
    KEY_UPARROW, KEY_DOWNARROW, KEY_LEFTARROW, KEY_RIGHTARROW,
    KEY_FIRE, KEY_USE, KEY_ESCAPE, KEY_TAB, KEY_ENTER,
};
#define NMANAGED ((int)(sizeof(g_managed) / sizeof(g_managed[0])))

static int g_touch_down[NMANAGED];
static int g_weapon = 1;        /* next weapon selected by the ARMA button */

static int g_queue[QUEUE_LEN];
static unsigned g_qhead = 0, g_qtail = 0;

/* ------------------------------------------------------------------ */
/* Key queue                                                           */
/* ------------------------------------------------------------------ */

static void push_key(int pressed, int key)
{
    unsigned next = (g_qtail + 1) % QUEUE_LEN;

    if (next == g_qhead)
        return; /* queue full: drop, never block the renderer */

    g_queue[g_qtail] = ((pressed ? 1 : 0) << 8) | (key & 0xff);
    g_qtail = next;
}

/* ------------------------------------------------------------------ */
/* Framebuffer helpers                                                 */
/* ------------------------------------------------------------------ */

static int read_rotation(void)
{
    const char *path = "/var/lib/revive/rotation";
    FILE *f = fopen(path, "r");
    int rot = 270;

    if (f) {
        if (fscanf(f, "%d", &rot) != 1)
            rot = 270;
        fclose(f);
    }
    if (rot != 90 && rot != 270) {
        fprintf(stderr, "revive-doom: invalid rotation '%d', using 270\n", rot);
        rot = 270;
    }
    return rot;
}

static void fb_init(void)
{
    const char *path = getenv("REVIVE_FB");

    if (!path || !*path)
        path = "/dev/fb0";

    g_fb_fd = open(path, O_RDWR);
    if (g_fb_fd < 0) {
        fprintf(stderr, "revive-doom: open %s: %s\n", path, strerror(errno));
        exit(1);
    }
    if (ioctl(g_fb_fd, FBIOGET_VSCREENINFO, &g_vi) < 0) {
        fprintf(stderr, "revive-doom: FBIOGET_VSCREENINFO: %s\n", strerror(errno));
        exit(1);
    }
    if (ioctl(g_fb_fd, FBIOGET_FSCREENINFO, &g_fi) < 0) {
        fprintf(stderr, "revive-doom: FBIOGET_FSCREENINFO: %s\n", strerror(errno));
        exit(1);
    }

    g_pw = (int)g_vi.xres;
    g_ph = (int)g_vi.yres;
    g_bpp = (int)g_vi.bits_per_pixel;

    if (g_bpp != 24 && g_bpp != 32) {
        fprintf(stderr, "revive-doom: unsupported %d bpp framebuffer\n", g_bpp);
        exit(1);
    }

    g_line_len = (int)g_fi.line_length;
    if (g_line_len <= 0)
        g_line_len = g_pw * (g_bpp / 8);

    g_fb_len = (size_t)g_line_len * (size_t)g_ph;
    g_fb_mem = mmap(NULL, g_fb_len, PROT_READ | PROT_WRITE, MAP_SHARED, g_fb_fd, 0);
    if (g_fb_mem == MAP_FAILED) {
        fprintf(stderr, "revive-doom: mmap %s: %s\n", path, strerror(errno));
        g_fb_mem = NULL;
        exit(1);
    }

    if (g_bpp == 32 && g_vi.transp.length == 8)
        g_alpha = 0xff000000u;

    g_rot = read_rotation();
    if (g_pw > g_ph) {
        fprintf(stderr, "revive-doom: framebuffer already landscape, no rotation\n");
        g_rot = 0;
    }

    if (g_rot == 0) {
        g_lw = g_pw;
        g_lh = g_ph;
    } else {
        g_lw = g_ph;
        g_lh = g_pw;
    }

    fprintf(stderr,
            "revive-doom: fb %dx%d %dbpp line_length %d -> landscape %dx%d rot %d\n",
            g_pw, g_ph, g_bpp, g_line_len, g_lw, g_lh, g_rot);
}

/* ------------------------------------------------------------------ */
/* Overlay drawing (once, at startup)                                  */
/* ------------------------------------------------------------------ */

static void mset(int vx, int vy)
{
    int x = (int)((long)vx * g_lw / LAND_W);
    int y = (int)((long)vy * g_lh / LAND_H);

    if (x >= 0 && y >= 0 && x < g_lw && y < g_lh)
        g_mask[(size_t)y * g_lw + x] = 1;
}

static void mring(int cx, int cy, int r, int t)
{
    int ro = r + t / 2;
    int ri = r - t / 2;

    if (ri < 0)
        ri = 0;

    for (int y = cy - ro - 1; y <= cy + ro + 1; y++) {
        for (int x = cx - ro - 1; x <= cx + ro + 1; x++) {
            int dx = x - cx, dy = y - cy;
            long d2 = (long)dx * dx + (long)dy * dy;
            if (d2 <= (long)ro * ro && d2 >= (long)ri * ri)
                mset(x, y);
        }
    }
}

static void mdisc(int cx, int cy, int r)
{
    for (int y = cy - r; y <= cy + r; y++) {
        for (int x = cx - r; x <= cx + r; x++) {
            int dx = x - cx, dy = y - cy;
            if ((long)dx * dx + (long)dy * dy <= (long)r * r)
                mset(x, y);
        }
    }
}

static void mrect(int x0, int y0, int x1, int y1, int t)
{
    for (int y = y0; y <= y1; y++) {
        for (int x = x0; x <= x1; x++) {
            if (x < x0 + t || x > x1 - t || y < y0 + t || y > y1 - t)
                mset(x, y);
        }
    }
}

static void mfill(int x0, int y0, int x1, int y1)
{
    for (int y = y0; y <= y1; y++)
        for (int x = x0; x <= x1; x++)
            mset(x, y);
}

static long edge(int ax, int ay, int bx, int by, int px, int py)
{
    return (long)(bx - ax) * (py - ay) - (long)(by - ay) * (px - ax);
}

static void mtri(int x0, int y0, int x1, int y1, int x2, int y2)
{
    int minx = x0 < x1 ? x0 : x1;
    int maxx = x0 > x1 ? x0 : x1;
    int miny = y0 < y1 ? y0 : y1;
    int maxy = y0 > y1 ? y0 : y1;

    if (x2 < minx) minx = x2;
    if (x2 > maxx) maxx = x2;
    if (y2 < miny) miny = y2;
    if (y2 > maxy) maxy = y2;

    for (int y = miny; y <= maxy; y++) {
        for (int x = minx; x <= maxx; x++) {
            long e0 = edge(x0, y0, x1, y1, x, y);
            long e1 = edge(x1, y1, x2, y2, x, y);
            long e2 = edge(x2, y2, x0, y0, x, y);

            if ((e0 >= 0 && e1 >= 0 && e2 >= 0) ||
                (e0 <= 0 && e1 <= 0 && e2 <= 0))
                mset(x, y);
        }
    }
}

static void overlay_init(void)
{
    size_t px = (size_t)g_lw * (size_t)g_lh;

    g_mask = calloc(px, 1);
    g_land = malloc(px * sizeof(uint32_t));
    g_colmap = malloc((size_t)g_lw * sizeof(int));
    g_rowmap = malloc((size_t)g_lh * sizeof(int));
    if (!g_mask || !g_land || !g_colmap || !g_rowmap) {
        fprintf(stderr, "revive-doom: out of memory\n");
        exit(1);
    }

    /* Fit 640x400 into the landscape canvas, keeping the aspect ratio */
    g_dh = g_lh;
    g_dw = (int)((long)g_lh * DOOMGENERIC_RESX / DOOMGENERIC_RESY);
    if (g_dw > g_lw) {
        g_dw = g_lw;
        g_dh = (int)((long)g_lw * DOOMGENERIC_RESY / DOOMGENERIC_RESX);
    }
    g_dx0 = (g_lw - g_dw) / 2;
    g_dy0 = (g_lh - g_dh) / 2;

    for (int dx = 0; dx < g_dw; dx++)
        g_colmap[dx] = (int)((long)dx * DOOMGENERIC_RESX / g_dw);

    for (int dy = 0; dy < g_dh; dy++)
        g_rowmap[dy] = (int)((long)dy * DOOMGENERIC_RESY / g_dh);

    /* Directional pad */
    mring(DPAD_CX, DPAD_CY, DPAD_R, 10);
    mtri(DPAD_CX, DPAD_CY - 135, DPAD_CX - 38, DPAD_CY - 70, DPAD_CX + 38, DPAD_CY - 70);
    mtri(DPAD_CX - 135, DPAD_CY, DPAD_CX - 70, DPAD_CY - 38, DPAD_CX - 70, DPAD_CY + 38);
    mtri(DPAD_CX, DPAD_CY + 135, DPAD_CX + 38, DPAD_CY + 70, DPAD_CX - 38, DPAD_CY + 70);
    mtri(DPAD_CX + 135, DPAD_CY, DPAD_CX + 70, DPAD_CY + 38, DPAD_CX + 70, DPAD_CY - 38);

    /* ATIRAR: big circle with a trigger dot */
    mring(FIRE_CX, FIRE_CY, FIRE_R, 9);
    mdisc(FIRE_CX, FIRE_CY, 40);

    /* USAR: small circle with an inner ring */
    mring(USE_CX, USE_CY, USE_R, 8);
    mring(USE_CX, USE_CY, 34, 8);

    /* ARMA: circle with three magazine bars */
    mring(WEAPON_CX, WEAPON_CY, WEAPON_R, 8);
    mfill(WEAPON_CX - 24, WEAPON_CY - 26, WEAPON_CX - 14, WEAPON_CY + 26);
    mfill(WEAPON_CX - 5, WEAPON_CY - 26, WEAPON_CX + 5, WEAPON_CY + 26);
    mfill(WEAPON_CX + 14, WEAPON_CY - 26, WEAPON_CX + 24, WEAPON_CY + 26);

    /* Top bar rectangles */
    mrect(ESC_X0, ESC_Y0, ESC_X1, ESC_Y1, 6);
    mrect(MAP_X0, MAP_Y0, MAP_X1, MAP_Y1, 6);
    mrect(ENTER_X0, ENTER_Y0, ENTER_X1, ENTER_Y1, 6);

    /* Static background: black bars plus the baked overlay outside the
       game rectangle (the game area is redrawn every frame). */
    for (int y = 0; y < g_lh; y++) {
        for (int x = 0; x < g_lw; x++) {
            int inside = (x >= g_dx0 && x < g_dx0 + g_dw &&
                          y >= g_dy0 && y < g_dy0 + g_dh);

            if (inside)
                continue;
            g_land[(size_t)y * g_lw + x] =
                g_mask[(size_t)y * g_lw + x] ? OVERLAY_BLACK : 0;
        }
    }

    fprintf(stderr, "revive-doom: doom rect %dx%d at %d,%d\n",
            g_dw, g_dh, g_dx0, g_dy0);
}

/* ------------------------------------------------------------------ */
/* Rendering                                                           */
/* ------------------------------------------------------------------ */

static inline uint32_t blend_white(uint32_t c)
{
    uint32_t r = (c >> 16) & 0xff;
    uint32_t g = (c >> 8) & 0xff;
    uint32_t b = c & 0xff;

    r += ((255 - r) * OVERLAY_ALPHA) >> 8;
    g += ((255 - g) * OVERLAY_ALPHA) >> 8;
    b += ((255 - b) * OVERLAY_ALPHA) >> 8;

    return (r << 16) | (g << 8) | b;
}

static void render_scale(void)
{
    const uint32_t *src = DG_ScreenBuffer;

    if (!src)
        return;

    for (int dy = 0; dy < g_dh; dy++) {
        const uint32_t *srow = src + (size_t)g_rowmap[dy] * DOOMGENERIC_RESX;
        uint32_t *drow = g_land + (size_t)(g_dy0 + dy) * g_lw + g_dx0;
        const uint8_t *mrow = g_mask + (size_t)(g_dy0 + dy) * g_lw + g_dx0;

        for (int dx = 0; dx < g_dw; dx++) {
            uint32_t c = srow[g_colmap[dx]];

            if (mrow[dx])
                c = blend_white(c);
            drow[dx] = c;
        }
    }
}

/* One rotation pass: 0x00RRGGBB landscape canvas -> framebuffer */
static void present(void)
{
    if (!g_fb_mem || !g_land)
        return;

    if (g_rot == 0) {
        if (g_bpp == 32) {
            for (int y = 0; y < g_lh; y++) {
                uint32_t *o = (uint32_t *)(g_fb_mem + (size_t)y * g_line_len);
                const uint32_t *s = g_land + (size_t)y * g_lw;
                for (int x = 0; x < g_lw; x++)
                    o[x] = s[x] | g_alpha;
            }
        } else {
            for (int y = 0; y < g_lh; y++) {
                uint8_t *o = g_fb_mem + (size_t)y * g_line_len;
                const uint32_t *s = g_land + (size_t)y * g_lw;
                for (int x = 0; x < g_lw; x++) {
                    uint32_t c = s[x];
                    o[x * 3 + 0] = (uint8_t)(c);
                    o[x * 3 + 1] = (uint8_t)(c >> 8);
                    o[x * 3 + 2] = (uint8_t)(c >> 16);
                }
            }
        }
        return;
    }

    for (int py = 0; py < g_ph; py++) {
        int x = (g_rot == 90) ? (g_ph - 1 - py) : py;
        const uint32_t *col = g_land + x;

        if (g_bpp == 32) {
            uint32_t *o = (uint32_t *)(g_fb_mem + (size_t)py * g_line_len);

            if (g_rot == 90) {
                const uint32_t *s = col;
                for (int px = 0; px < g_pw; px++, s += g_lw)
                    o[px] = *s | g_alpha;
            } else {
                const uint32_t *s = col + (size_t)(g_pw - 1) * g_lw;
                for (int px = 0; px < g_pw; px++, s -= g_lw)
                    o[px] = *s | g_alpha;
            }
        } else {
            uint8_t *o = g_fb_mem + (size_t)py * g_line_len;

            if (g_rot == 90) {
                const uint32_t *s = col;
                for (int px = 0; px < g_pw; px++, s += g_lw) {
                    uint32_t c = *s;
                    o[px * 3 + 0] = (uint8_t)(c);
                    o[px * 3 + 1] = (uint8_t)(c >> 8);
                    o[px * 3 + 2] = (uint8_t)(c >> 16);
                }
            } else {
                const uint32_t *s = col + (size_t)(g_pw - 1) * g_lw;
                for (int px = 0; px < g_pw; px++, s -= g_lw) {
                    uint32_t c = *s;
                    o[px * 3 + 0] = (uint8_t)(c);
                    o[px * 3 + 1] = (uint8_t)(c >> 8);
                    o[px * 3 + 2] = (uint8_t)(c >> 16);
                }
            }
        }
    }
}

/* ------------------------------------------------------------------ */
/* Input: evdev                                                       */
/* ------------------------------------------------------------------ */

#define NLONGS(bits) (((bits) + 8 * sizeof(unsigned long) - 1) / (8 * sizeof(unsigned long)))

static int has_bit(const unsigned long *arr, int bit)
{
    return (int)((arr[bit / (8 * sizeof(unsigned long))] >>
                  (bit % (8 * sizeof(unsigned long)))) & 1UL);
}

static int map_key(unsigned code)
{
    switch (code) {
    case KEY_UP:        return KEY_UPARROW;
    case KEY_DOWN:      return KEY_DOWNARROW;
    case KEY_LEFT:      return KEY_LEFTARROW;
    case KEY_RIGHT:     return KEY_RIGHTARROW;
    case KEY_LEFTCTRL:
    case KEY_RIGHTCTRL: return KEY_FIRE;
    case KEY_SPACE:     return KEY_USE;
    case KEY_ESC:       return KEY_ESCAPE;
    case EVDEV_ENTER:   return KEY_ENTER;
    case EVDEV_TAB:     return KEY_TAB;
    case KEY_LEFTSHIFT:
    case KEY_RIGHTSHIFT: return KEY_RSHIFT;
    case KEY_LEFTALT:
    case KEY_RIGHTALT:  return KEY_LALT;
    case KEY_1: return '1';
    case KEY_2: return '2';
    case KEY_3: return '3';
    case KEY_4: return '4';
    case KEY_5: return '5';
    case KEY_6: return '6';
    case KEY_7: return '7';
    case KEY_Y: return 'y';
    case KEY_N: return 'n';
    /* gamepad */
    case BTN_SOUTH: return KEY_FIRE;
    case BTN_EAST:  return KEY_USE;
    case BTN_START: return KEY_ESCAPE;
    default: return -1;
    }
}

/* physical -> logical landscape -> virtual 1280x720 layout */
static void phys_to_touch(int px, int py, int *tx, int *ty)
{
    int x, y;

    if (g_rot == 90) {
        x = g_ph - 1 - py;
        y = px;
    } else if (g_rot == 270) {
        x = py;
        y = g_pw - 1 - px;
    } else {
        x = px;
        y = py;
    }

    *tx = (int)((long)x * LAND_W / g_lw);
    *ty = (int)((long)y * LAND_H / g_lh);
}

static int in_rect(int x, int y, int x0, int y0, int x1, int y1)
{
    return x >= x0 && x <= x1 && y >= y0 && y <= y1;
}

static long dist2(int x, int y, int cx, int cy)
{
    long dx = x - cx, dy = y - cy;

    return dx * dx + dy * dy;
}

/* Which keys does a finger at (x,y) hold?  Returns the count, 0..2. */
static int touch_hit(int x, int y, int *keys, int *weapon)
{
    *weapon = 0;

    if (in_rect(x, y, ESC_X0, ESC_Y0, ESC_X1, ESC_Y1)) {
        keys[0] = KEY_ESCAPE;
        return 1;
    }
    if (in_rect(x, y, MAP_X0, MAP_Y0, MAP_X1, MAP_Y1)) {
        keys[0] = KEY_TAB;
        return 1;
    }
    if (in_rect(x, y, ENTER_X0, ENTER_Y0, ENTER_X1, ENTER_Y1)) {
        keys[0] = KEY_ENTER;
        return 1;
    }
    if (dist2(x, y, FIRE_CX, FIRE_CY) <= (long)FIRE_R * FIRE_R) {
        keys[0] = KEY_FIRE;
        return 1;
    }
    if (dist2(x, y, USE_CX, USE_CY) <= (long)USE_R * USE_R) {
        keys[0] = KEY_USE;
        return 1;
    }
    if (dist2(x, y, WEAPON_CX, WEAPON_CY) <= (long)WEAPON_R * WEAPON_R) {
        *weapon = 1;
        return 0;
    }

    {
        int dx = x - DPAD_CX;
        int dy = y - DPAD_CY;
        int ax, ay, n = 0;

        if ((long)dx * dx + (long)dy * dy > (long)DPAD_R * DPAD_R)
            return 0;
        if ((long)dx * dx + (long)dy * dy < DEADZONE2)
            return 0;

        ax = dx < 0 ? -dx : dx;
        ay = dy < 0 ? -dy : dy;

        if (ax * 2 > ay)
            keys[n++] = dx < 0 ? KEY_LEFTARROW : KEY_RIGHTARROW;
        if (ay * 2 > ax)
            keys[n++] = dy < 0 ? KEY_UPARROW : KEY_DOWNARROW;

        if (n == 0)
            keys[n++] = ax > ay
                ? (dx < 0 ? KEY_LEFTARROW : KEY_RIGHTARROW)
                : (dy < 0 ? KEY_UPARROW : KEY_DOWNARROW);

        return n;
    }
}

static int key_index(int key)
{
    for (int i = 0; i < NMANAGED; i++)
        if (g_managed[i] == key)
            return i;
    return -1;
}

static void update_touch(void)
{
    int want[NMANAGED];
    int dirty_weapon = 0;

    memset(want, 0, sizeof(want));

    for (int d = 0; d < g_ndev; d++) {
        struct input_dev *dv = &g_dev[d];

        for (int s = 0; s < MAX_SLOTS; s++) {
            struct touch_slot *slot = &dv->slots[s];
            int keys[2];
            int weapon = 0;
            int n, tx, ty;

            if (!slot->active) {
                slot->weapon = 0;
                continue;
            }

            phys_to_touch(slot->x, slot->y, &tx, &ty);
            n = touch_hit(tx, ty, keys, &weapon);

            for (int i = 0; i < n; i++) {
                int idx = key_index(keys[i]);

                if (idx >= 0)
                    want[idx] = 1;
            }

            if (weapon && !slot->weapon)
                dirty_weapon = 1;
            slot->weapon = weapon;
        }
    }

    if (dirty_weapon) {
        int key = '1' + (g_weapon - 1);

        push_key(1, key);
        push_key(0, key);
        g_weapon = (g_weapon % 7) + 1;
    }

    for (int i = 0; i < NMANAGED; i++) {
        if (want[i] != g_touch_down[i]) {
            push_key(want[i], g_managed[i]);
            g_touch_down[i] = want[i];
        }
    }
}

static void hat_key(struct input_dev *dv, int axis, int value,
                    int neg_key, int pos_key)
{
    int old = dv->hat[axis];

    if (old == value)
        return;

    if (old < 0)
        push_key(0, neg_key);
    if (old > 0)
        push_key(0, pos_key);

    dv->hat[axis] = value;

    if (value < 0)
        push_key(1, neg_key);
    if (value > 0)
        push_key(1, pos_key);
}

static void dev_abs(struct input_dev *dv, const struct input_event *ev)
{
    int code = (int)ev->code;
    int val = (int)ev->value;
    struct touch_slot *slot = &dv->slots[dv->cur_slot];

    switch (code) {
    case ABS_MT_SLOT:
        dv->has_slot = 1;
        if (val >= 0 && val < MAX_SLOTS)
            dv->cur_slot = val;
        break;

    case ABS_MT_TRACKING_ID:
        if (val < 0) {
            slot->active = 0;
            slot->weapon = 0;
        } else {
            slot->active = 1;
        }
        dv->dirty = 1;
        break;

    case ABS_MT_POSITION_X:
    case ABS_MT_POSITION_Y:
        if (code == ABS_MT_POSITION_X)
            slot->x = val;
        else
            slot->y = val;
        if (!dv->has_slot) {
            slot->active = 1;
            dv->cur_slot = 0;
        }
        dv->dirty = 1;
        break;

    case ABS_X:
        dv->slots[0].x = val;
        if (!dv->has_slot && dv->btn_touch)
            dv->slots[0].active = 1;
        dv->dirty = 1;
        break;

    case ABS_Y:
        dv->slots[0].y = val;
        if (!dv->has_slot && dv->btn_touch)
            dv->slots[0].active = 1;
        dv->dirty = 1;
        break;

    case ABS_HAT0X:
        hat_key(dv, 0, val, KEY_LEFTARROW, KEY_RIGHTARROW);
        break;

    case ABS_HAT0Y:
        hat_key(dv, 1, val, KEY_UPARROW, KEY_DOWNARROW);
        break;

    default:
        break;
    }
}

static void dev_key(struct input_dev *dv, const struct input_event *ev)
{
    int pressed = ev->value != 0;
    int key;

    if (ev->value == 2)
        return; /* autorepeat */

    if (ev->code == BTN_TOUCH) {
        dv->btn_touch = pressed;
        if (!dv->has_slot) {
            dv->slots[0].active = pressed;
            if (!pressed)
                dv->slots[0].weapon = 0;
        }
        dv->dirty = 1;
        return;
    }

    key = map_key(ev->code);
    if (key >= 0)
        push_key(pressed, key);
}

static void input_init(void)
{
    DIR *dir = opendir("/dev/input");
    struct dirent *de;

    if (!dir) {
        fprintf(stderr, "revive-doom: opendir /dev/input: %s\n", strerror(errno));
        return;
    }

    while ((de = readdir(dir)) != NULL && g_ndev < MAX_DEV) {
        unsigned long evbits[NLONGS(EV_MAX)];
        unsigned long keybits[NLONGS(KEY_MAX)];
        unsigned long absbits[NLONGS(ABS_MAX)];
        struct input_dev *dv;
        char path[300];
        int fd, touch, kbd;

        if (strncmp(de->d_name, "event", 5) != 0)
            continue;

        snprintf(path, sizeof(path), "/dev/input/%s", de->d_name);
        fd = open(path, O_RDONLY | O_NONBLOCK);
        if (fd < 0)
            continue;

        memset(evbits, 0, sizeof(evbits));
        memset(keybits, 0, sizeof(keybits));
        memset(absbits, 0, sizeof(absbits));

        if (ioctl(fd, EVIOCGBIT(0, sizeof(evbits)), evbits) < 0) {
            close(fd);
            continue;
        }

        if (has_bit(evbits, EV_KEY))
            ioctl(fd, EVIOCGBIT(EV_KEY, sizeof(keybits)), keybits);
        if (has_bit(evbits, EV_ABS))
            ioctl(fd, EVIOCGBIT(EV_ABS, sizeof(absbits)), absbits);

        touch = has_bit(absbits, ABS_MT_POSITION_X);
        kbd = has_bit(keybits, KEY_A) || has_bit(keybits, BTN_SOUTH);

        if (!touch && !kbd) {
            close(fd);
            continue;
        }

        if (ioctl(fd, EVIOCGRAB, 1) < 0)
            fprintf(stderr, "revive-doom: EVIOCGRAB %s: %s\n",
                    path, strerror(errno));

        dv = &g_dev[g_ndev++];
        memset(dv, 0, sizeof(*dv));
        dv->fd = fd;
        dv->touch = touch;
        dv->cur_slot = 0;
        dv->hat[0] = 0;
        dv->hat[1] = 0;

        fprintf(stderr, "revive-doom: input %s (%s)\n", path,
                touch ? "touchscreen" : "keyboard/gamepad");
    }

    closedir(dir);
}

static void poll_input(void)
{
    int dirty = 0;

    for (int i = 0; i < g_ndev; i++) {
        struct input_event ev;
        ssize_t n;

        while ((n = read(g_dev[i].fd, &ev, sizeof(ev))) == (ssize_t)sizeof(ev)) {
            if (ev.type == EV_ABS)
                dev_abs(&g_dev[i], &ev);
            else if (ev.type == EV_KEY)
                dev_key(&g_dev[i], &ev);
        }

        if (g_dev[i].dirty) {
            g_dev[i].dirty = 0;
            dirty = 1;
        }
    }

    if (dirty)
        update_touch();
}

/* ------------------------------------------------------------------ */
/* Cleanup                                                             */
/* ------------------------------------------------------------------ */

static void revive_cleanup(void)
{
    for (int i = 0; i < g_ndev; i++) {
        if (g_dev[i].fd >= 0) {
            ioctl(g_dev[i].fd, EVIOCGRAB, 0);
            close(g_dev[i].fd);
            g_dev[i].fd = -1;
        }
    }
    g_ndev = 0;

    if (g_fb_mem) {
        memset(g_fb_mem, 0, g_fb_len);
        munmap(g_fb_mem, g_fb_len);
        g_fb_mem = NULL;
    }
    if (g_fb_fd >= 0) {
        close(g_fb_fd);
        g_fb_fd = -1;
    }
}

/* ------------------------------------------------------------------ */
/* doomgeneric platform interface                                      */
/* ------------------------------------------------------------------ */

void DG_Init(void)
{
    fb_init();
    overlay_init();
    input_init();
    atexit(revive_cleanup);
}

void DG_DrawFrame(void)
{
    render_scale();
    present();
}

void DG_SleepMs(uint32_t ms)
{
    struct timespec ts;

    ts.tv_sec = (time_t)(ms / 1000);
    ts.tv_nsec = (long)(ms % 1000) * 1000000L;
    nanosleep(&ts, NULL);
}

uint32_t DG_GetTicksMs(void)
{
    struct timespec ts;

    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint32_t)((uint64_t)ts.tv_sec * 1000u +
                      (uint64_t)ts.tv_nsec / 1000000u);
}

int DG_GetKey(int *pressed, unsigned char *key)
{
    if (g_qhead == g_qtail)
        poll_input();

    if (g_qhead == g_qtail)
        return 0;

    {
        int v = g_queue[g_qhead];

        g_qhead = (g_qhead + 1) % QUEUE_LEN;
        *pressed = v >> 8;
        *key = (unsigned char)(v & 0xff);
    }

    return 1;
}

void DG_SetWindowTitle(const char *title)
{
    (void)title;
    /* No window manager on a framebuffer. */
}

int main(int argc, char **argv)
{
    doomgeneric_Create(argc, argv);

    for (;;)
        doomgeneric_Tick();

    return 0;
}
