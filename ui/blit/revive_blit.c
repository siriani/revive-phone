// SPDX-License-Identifier: MIT
/*
 * Revive: copia uma imagem RGBA (desenhada em paisagem) para o framebuffer
 * (retrato), girando 90/270 graus e convertendo o formato.
 *
 * Em Python (Pillow) a tela inteira custava ~95 ms num Cortex-A7; aqui ~25 ms,
 * e um botão menos de 1 ms. Percorre em blocos de 32x32 para aproveitar o cache:
 * cada coluna da paisagem vira um trecho contínuo de uma linha do framebuffer.
 */
#include <stdint.h>

#define BLOCO 32

static inline int min_i(int a, int b) { return a < b ? a : b; }

#define COLUNA(ESCREVE)							\
	for (y = by; y < ey; y++, s += src_stride, d += passo) {	\
		ESCREVE;						\
	}

/*
 * src:        RGBA 8888 (bytes R,G,B,A), sw x sh, src_stride bytes por linha
 * ox, oy:     onde o canto da imagem cai na tela (paisagem)
 * scr_w/h:    tamanho da tela em paisagem (ex.: 1280 x 720)
 * dst:        framebuffer, dst_stride bytes por linha
 * bpp:        32 (bytes B,G,R,X), 24 (bytes B,G,R) ou 16 (RGB565)
 * rot:        90  -> ponto (x, y) da paisagem vai para (y, scr_w-1-x) do retrato
 *             270 -> ponto (x, y) vai para (scr_h-1-y, x)
 *             0   -> sem giro (framebuffer já em paisagem)
 */
void revive_blit(const uint8_t *src, int sw, int sh, int src_stride,
		 int ox, int oy, int scr_w, int scr_h,
		 uint8_t *dst, int dst_stride, int bpp, int rot)
{
	int bx, by, x, y;
	int bytes = bpp / 8;

	/* recorta o que cair fora da tela */
	if (ox < 0) { src -= ox * 4; sw += ox; ox = 0; }
	if (oy < 0) { src -= oy * src_stride; sh += oy; oy = 0; }
	sw = min_i(sw, scr_w - ox);
	sh = min_i(sh, scr_h - oy);
	if (sw <= 0 || sh <= 0)
		return;

	for (by = 0; by < sh; by += BLOCO) {
		int ey = min_i(by + BLOCO, sh);
		for (bx = 0; bx < sw; bx += BLOCO) {
			int ex = min_i(bx + BLOCO, sw);
			for (x = bx; x < ex; x++) {
				const uint8_t *s = src + by * src_stride + x * 4;
				int lx = ox + x, ly = oy + by;	/* ponto na paisagem */
				uint8_t *d;
				int passo;

				if (rot == 90) {
					d = dst + (scr_w - 1 - lx) * dst_stride + ly * bytes;
					passo = bytes;
				} else if (rot == 270) {
					d = dst + lx * dst_stride + (scr_h - 1 - ly) * bytes;
					passo = -bytes;
				} else {
					d = dst + ly * dst_stride + lx * bytes;
					passo = dst_stride;
				}
				if (bytes == 4) {
					/* RGBA lido como uint32 (little-endian) = A B G R; troca R e B */
					COLUNA(uint32_t p = *(const uint32_t *)s;
					       *(uint32_t *)d = 0xff000000u | (p & 0x0000ff00u) |
								((p & 0xffu) << 16) | ((p >> 16) & 0xffu))
				} else if (bytes == 3) {
					COLUNA(d[0] = s[2]; d[1] = s[1]; d[2] = s[0])
				} else {
					COLUNA(*(uint16_t *)d = ((s[0] & 0xf8) << 8) |
								((s[1] & 0xfc) << 3) | (s[2] >> 3))
				}
			}
		}
	}
}
