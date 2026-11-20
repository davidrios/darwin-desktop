#pragma once

#include <stddef.h>
#include <stdint.h>

/* FNV-1a over the visible pixels of an XRGB/ARGB8888 buffer, row by row, so
 * client and server agree regardless of stride padding. */
static inline uint32_t spike_checksum(const void *data, int32_t width, int32_t height, int32_t stride)
{
	uint32_t h = 2166136261u;
	const uint8_t *base = data;
	for (int32_t y = 0; y < height; y++) {
		const uint8_t *row = base + (size_t)y * (size_t)stride;
		for (int32_t i = 0; i < width * 4; i++) {
			h ^= row[i];
			h *= 16777619u;
		}
	}
	return h;
}

static inline uint32_t spike_pixel(int32_t x, int32_t y)
{
	return 0xff000000u | ((uint32_t)(x & 0xff) << 16) | ((uint32_t)(y & 0xff) << 8) |
	       ((uint32_t)(x ^ y) & 0xff);
}
