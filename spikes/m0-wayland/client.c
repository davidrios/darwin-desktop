/* M0 spike: a minimal wl_shm client on Darwin.
 *
 * Binds wl_compositor and wl_shm, fills a WIDTHxHEIGHT XRGB8888 buffer with a
 * known pattern, attaches and commits it, then waits for the frame callback and
 * buffer release. Prints the checksum it expects the server to see.
 *
 * Darwin has no memfd_create or posix_fallocate, so the pool is a POSIX shm
 * object (shm_open + shm_unlink + ftruncate), passed to the server as an fd.
 */
#include <errno.h>
#include <fcntl.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#include <wayland-client.h>

#include "common.h"

#define WIDTH 256
#define HEIGHT 192
#define STRIDE (WIDTH * 4)
#define SIZE (STRIDE * HEIGHT)

static struct wl_compositor *compositor;
static struct wl_shm *shm;
static bool have_xrgb;
static bool frame_done;
static bool buffer_released;

static void shm_format(void *data, struct wl_shm *s, uint32_t format)
{
	(void)data; (void)s;
	if (format == WL_SHM_FORMAT_XRGB8888)
		have_xrgb = true;
}

static const struct wl_shm_listener shm_listener = { .format = shm_format };

static void registry_global(void *data, struct wl_registry *reg, uint32_t name, const char *iface, uint32_t version)
{
	(void)data;
	if (strcmp(iface, wl_compositor_interface.name) == 0) {
		compositor = wl_registry_bind(reg, name, &wl_compositor_interface, version < 4 ? version : 4);
	} else if (strcmp(iface, wl_shm_interface.name) == 0) {
		shm = wl_registry_bind(reg, name, &wl_shm_interface, 1);
		wl_shm_add_listener(shm, &shm_listener, NULL);
	}
}

static void registry_global_remove(void *data, struct wl_registry *reg, uint32_t name)
{ (void)data; (void)reg; (void)name; }

static const struct wl_registry_listener registry_listener = {
	.global = registry_global,
	.global_remove = registry_global_remove,
};

static void buffer_release(void *data, struct wl_buffer *b) { (void)data; (void)b; buffer_released = true; }
static const struct wl_buffer_listener buffer_listener = { .release = buffer_release };

static void frame_callback_done(void *data, struct wl_callback *cb, uint32_t time)
{
	(void)data; (void)time;
	frame_done = true;
	wl_callback_destroy(cb);
}
static const struct wl_callback_listener frame_listener = { .done = frame_callback_done };

static int create_shm_fd(size_t size)
{
	char name[32];
	for (int attempt = 0; attempt < 100; attempt++) {
		/* PSHMNAMLEN on Darwin is 31 characters. */
		snprintf(name, sizeof(name), "/wlspike-%d-%d", (int)getpid(), attempt);
		int fd = shm_open(name, O_RDWR | O_CREAT | O_EXCL, 0600);
		if (fd >= 0) {
			shm_unlink(name);
			if (ftruncate(fd, (off_t)size) != 0) {
				perror("client: ftruncate");
				close(fd);
				return -1;
			}
			return fd;
		}
		if (errno != EEXIST) {
			perror("client: shm_open");
			return -1;
		}
	}
	return -1;
}

int main(void)
{
	struct wl_display *display = wl_display_connect(NULL);
	if (!display) {
		perror("client: wl_display_connect");
		return 1;
	}

	struct wl_registry *registry = wl_display_get_registry(display);
	wl_registry_add_listener(registry, &registry_listener, NULL);
	wl_display_roundtrip(display); /* globals */
	wl_display_roundtrip(display); /* wl_shm formats */

	if (!compositor || !shm) {
		fprintf(stderr, "client: missing globals (compositor=%p shm=%p)\n", (void *)compositor, (void *)shm);
		return 1;
	}
	if (!have_xrgb) {
		fprintf(stderr, "client: server did not advertise XRGB8888\n");
		return 1;
	}

	int fd = create_shm_fd(SIZE);
	if (fd < 0)
		return 1;
	uint32_t *pixels = mmap(NULL, SIZE, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (pixels == MAP_FAILED) {
		perror("client: mmap");
		return 1;
	}
	for (int y = 0; y < HEIGHT; y++)
		for (int x = 0; x < WIDTH; x++)
			pixels[y * WIDTH + x] = spike_pixel(x, y);
	uint32_t sum = spike_checksum(pixels, WIDTH, HEIGHT, STRIDE);

	struct wl_shm_pool *pool = wl_shm_create_pool(shm, fd, SIZE);
	struct wl_buffer *buffer = wl_shm_pool_create_buffer(pool, 0, WIDTH, HEIGHT, STRIDE, WL_SHM_FORMAT_XRGB8888);
	wl_buffer_add_listener(buffer, &buffer_listener, NULL);
	wl_shm_pool_destroy(pool);
	close(fd);

	struct wl_surface *surface = wl_compositor_create_surface(compositor);
	wl_surface_attach(surface, buffer, 0, 0);
	wl_surface_damage_buffer(surface, 0, 0, WIDTH, HEIGHT);
	struct wl_callback *cb = wl_surface_frame(surface);
	wl_callback_add_listener(cb, &frame_listener, NULL);
	wl_surface_commit(surface);

	printf("client: committed %dx%d XRGB8888 buffer checksum=%08x\n", WIDTH, HEIGHT, sum);
	fflush(stdout);

	while (!(frame_done && buffer_released)) {
		if (wl_display_dispatch(display) < 0) {
			fprintf(stderr, "client: connection error: %s\n", strerror(errno));
			return 1;
		}
	}
	printf("client: frame done, buffer released\n");

	wl_buffer_destroy(buffer);
	wl_surface_destroy(surface);
	munmap(pixels, SIZE);
	wl_shm_destroy(shm);
	wl_compositor_destroy(compositor);
	wl_registry_destroy(registry);
	wl_display_disconnect(display);
	return 0;
}
