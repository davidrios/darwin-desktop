/* M0 spike: a minimal Wayland server on Darwin.
 *
 * Exposes wl_compositor (surfaces and regions) and libwayland's built-in wl_shm.
 * On each surface commit with a buffer attached, it reads the shm buffer,
 * prints its size, format and checksum, releases the buffer and fires frame
 * callbacks. With --once it exits after the first client disconnects.
 *
 * Usage: wl-spike-server [--once] [socket-name]
 */
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <wayland-server.h>

#include "common.h"

struct surface {
	struct wl_resource *resource;
	struct wl_resource *pending_buffer;
	bool has_pending_buffer;
	struct wl_list frame_callbacks; /* wl_resource link */
};

static bool exit_after_client;
static struct wl_display *display;
static unsigned long commits_with_buffer;

/* ---- wl_region: accepted and ignored ---- */

static void region_destroy(struct wl_client *c, struct wl_resource *r) { (void)c; wl_resource_destroy(r); }
static void region_add(struct wl_client *c, struct wl_resource *r, int32_t x, int32_t y, int32_t w, int32_t h)
{ (void)c; (void)r; (void)x; (void)y; (void)w; (void)h; }

static const struct wl_region_interface region_impl = {
	.destroy = region_destroy,
	.add = region_add,
	.subtract = region_add,
};

/* ---- wl_surface ---- */

static void surface_destroy(struct wl_client *c, struct wl_resource *r) { (void)c; wl_resource_destroy(r); }

static void surface_attach(struct wl_client *c, struct wl_resource *r, struct wl_resource *buffer, int32_t x, int32_t y)
{
	(void)c; (void)x; (void)y;
	struct surface *s = wl_resource_get_user_data(r);
	s->pending_buffer = buffer;
	s->has_pending_buffer = true;
}

static void surface_damage(struct wl_client *c, struct wl_resource *r, int32_t x, int32_t y, int32_t w, int32_t h)
{ (void)c; (void)r; (void)x; (void)y; (void)w; (void)h; }

static void callback_unlink(struct wl_resource *r) { wl_list_remove(wl_resource_get_link(r)); }

static void surface_frame(struct wl_client *c, struct wl_resource *r, uint32_t id)
{
	struct surface *s = wl_resource_get_user_data(r);
	struct wl_resource *cb = wl_resource_create(c, &wl_callback_interface, 1, id);
	if (!cb) {
		wl_client_post_no_memory(c);
		return;
	}
	wl_resource_set_implementation(cb, NULL, NULL, callback_unlink);
	wl_list_insert(s->frame_callbacks.prev, wl_resource_get_link(cb));
}

static void surface_set_region(struct wl_client *c, struct wl_resource *r, struct wl_resource *region)
{ (void)c; (void)r; (void)region; }

static void surface_commit(struct wl_client *c, struct wl_resource *r)
{
	(void)c;
	struct surface *s = wl_resource_get_user_data(r);

	if (s->has_pending_buffer && s->pending_buffer) {
		struct wl_shm_buffer *shm = wl_shm_buffer_get(s->pending_buffer);
		if (shm) {
			int32_t w = wl_shm_buffer_get_width(shm);
			int32_t h = wl_shm_buffer_get_height(shm);
			int32_t stride = wl_shm_buffer_get_stride(shm);
			uint32_t fmt = wl_shm_buffer_get_format(shm);
			wl_shm_buffer_begin_access(shm);
			uint32_t sum = spike_checksum(wl_shm_buffer_get_data(shm), w, h, stride);
			wl_shm_buffer_end_access(shm);
			commits_with_buffer++;
			printf("server: commit %lu: shm buffer %dx%d stride=%d format=0x%x checksum=%08x\n",
			       commits_with_buffer, w, h, stride, fmt, sum);
			fflush(stdout);
		} else {
			printf("server: commit with a non-shm buffer (ignored)\n");
		}
		/* Contents are consumed immediately; the client may reuse the buffer. */
		wl_buffer_send_release(s->pending_buffer);
	}
	s->has_pending_buffer = false;
	s->pending_buffer = NULL;

	struct wl_resource *cb, *tmp;
	wl_resource_for_each_safe(cb, tmp, &s->frame_callbacks) {
		wl_callback_send_done(cb, (uint32_t)(wl_display_get_serial(display) & 0xffffffffu));
		wl_resource_destroy(cb);
	}
}

static void surface_set_int(struct wl_client *c, struct wl_resource *r, int32_t v) { (void)c; (void)r; (void)v; }

static const struct wl_surface_interface surface_impl = {
	.destroy = surface_destroy,
	.attach = surface_attach,
	.damage = surface_damage,
	.frame = surface_frame,
	.set_opaque_region = surface_set_region,
	.set_input_region = surface_set_region,
	.commit = surface_commit,
	.set_buffer_transform = surface_set_int,
	.set_buffer_scale = surface_set_int,
	.damage_buffer = surface_damage,
};

static void surface_resource_destroy(struct wl_resource *r)
{
	struct surface *s = wl_resource_get_user_data(r);
	struct wl_resource *cb, *tmp;
	wl_resource_for_each_safe(cb, tmp, &s->frame_callbacks)
		wl_resource_destroy(cb);
	free(s);
}

/* ---- wl_compositor ---- */

static void compositor_create_surface(struct wl_client *c, struct wl_resource *r, uint32_t id)
{
	struct surface *s = calloc(1, sizeof(*s));
	if (!s) {
		wl_client_post_no_memory(c);
		return;
	}
	s->resource = wl_resource_create(c, &wl_surface_interface, wl_resource_get_version(r), id);
	if (!s->resource) {
		free(s);
		wl_client_post_no_memory(c);
		return;
	}
	wl_list_init(&s->frame_callbacks);
	wl_resource_set_implementation(s->resource, &surface_impl, s, surface_resource_destroy);
}

static void compositor_create_region(struct wl_client *c, struct wl_resource *r, uint32_t id)
{
	struct wl_resource *region = wl_resource_create(c, &wl_region_interface, 1, id);
	(void)r;
	if (!region) {
		wl_client_post_no_memory(c);
		return;
	}
	wl_resource_set_implementation(region, &region_impl, NULL, NULL);
}

static const struct wl_compositor_interface compositor_impl = {
	.create_surface = compositor_create_surface,
	.create_region = compositor_create_region,
};

static void compositor_bind(struct wl_client *c, void *data, uint32_t version, uint32_t id)
{
	(void)data;
	struct wl_resource *r = wl_resource_create(c, &wl_compositor_interface, (int)version, id);
	if (!r) {
		wl_client_post_no_memory(c);
		return;
	}
	wl_resource_set_implementation(r, &compositor_impl, NULL, NULL);
}

/* ---- client lifecycle ---- */

static void client_destroyed(struct wl_listener *l, void *data)
{
	(void)data;
	printf("server: client disconnected\n");
	fflush(stdout);
	wl_list_remove(&l->link);
	free(l);
	if (exit_after_client)
		wl_display_terminate(display);
}

static void client_created(struct wl_listener *l, void *data)
{
	(void)l;
	struct wl_client *client = data;
	pid_t pid;
	uid_t uid;
	gid_t gid;
	wl_client_get_credentials(client, &pid, &uid, &gid);
	printf("server: client connected pid=%d uid=%d gid=%d\n", (int)pid, (int)uid, (int)gid);
	fflush(stdout);

	struct wl_listener *destroy = calloc(1, sizeof(*destroy));
	if (!destroy)
		return;
	destroy->notify = client_destroyed;
	wl_client_add_destroy_listener(client, destroy);
}

static struct wl_listener client_created_listener = { .notify = client_created };

int main(int argc, char **argv)
{
	const char *socket_name = NULL;
	for (int i = 1; i < argc; i++) {
		if (strcmp(argv[i], "--once") == 0)
			exit_after_client = true;
		else
			socket_name = argv[i];
	}

	display = wl_display_create();
	if (!display) {
		fprintf(stderr, "server: wl_display_create failed\n");
		return 1;
	}

	if (wl_display_init_shm(display) != 0) {
		fprintf(stderr, "server: wl_display_init_shm failed\n");
		return 1;
	}
	if (!wl_global_create(display, &wl_compositor_interface, 4, NULL, compositor_bind)) {
		fprintf(stderr, "server: creating wl_compositor global failed\n");
		return 1;
	}
	wl_display_add_client_created_listener(display, &client_created_listener);

	if (socket_name) {
		if (wl_display_add_socket(display, socket_name) != 0) {
			perror("server: wl_display_add_socket");
			return 1;
		}
	} else {
		socket_name = wl_display_add_socket_auto(display);
		if (!socket_name) {
			perror("server: wl_display_add_socket_auto");
			return 1;
		}
	}
	printf("server: listening on %s\n", socket_name);
	fflush(stdout);

	wl_display_run(display);

	printf("server: exiting after %lu buffer commit(s)\n", commits_with_buffer);
	wl_display_destroy_clients(display);
	wl_display_destroy(display);
	return 0;
}
