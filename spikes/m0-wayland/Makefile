PKG_CONFIG ?= pkg-config
CFLAGS ?= -O2 -g
CFLAGS += -std=c11 -Wall -Wextra -Werror
PREFIX ?= /usr/local

all: wl-spike-server wl-spike-client

wl-spike-server: server.c common.h
	$(CC) $(CFLAGS) $$($(PKG_CONFIG) --cflags wayland-server) -o $@ server.c $(LDFLAGS) $$($(PKG_CONFIG) --libs wayland-server)

wl-spike-client: client.c common.h
	$(CC) $(CFLAGS) $$($(PKG_CONFIG) --cflags wayland-client) -o $@ client.c $(LDFLAGS) $$($(PKG_CONFIG) --libs wayland-client)

install: all
	install -d $(PREFIX)/bin
	install -m755 wl-spike-server wl-spike-client run-spike.sh $(PREFIX)/bin/

clean:
	rm -f wl-spike-server wl-spike-client

.PHONY: all install clean
