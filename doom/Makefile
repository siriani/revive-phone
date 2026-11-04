# Revive Phone - doomgeneric for the phone framebuffer
#
# Cross-compile (default):  make
#   zig cc -target arm-linux-musleabihf -O2 -mcpu=cortex_a7
# Host syntax check:        make nativo
# Override the compiler:    make CC="clang --target=arm-linux-gnueabihf"
#
# SPDX-License-Identifier: GPL-2.0-only

ENGINE_DIR = engine
REVIVE_DIR = revive
OBJDIR    ?= build
OUTPUT    ?= revive-doom
CROSS_CC   = zig cc -target arm-linux-musleabihf -O2 -mcpu=cortex_a7

# Only use the ARM cross compiler when CC was not given explicitly.
ifeq ($(origin CC),default)
CC = $(CROSS_CC)
endif

CFLAGS ?= -O2 -Wall -DNORMALUNIX -DLINUX -D_DEFAULT_SOURCE \
          -DDOOMGENERIC_RESX=640 -DDOOMGENERIC_RESY=400 \
          -I$(ENGINE_DIR)
LIBS   ?= -lm

SRC_ENGINE = \
	dummy am_map doomdef doomstat dstrings d_event d_items d_iwad d_loop \
	d_main d_mode d_net f_finale f_wipe g_game hu_lib hu_stuff info \
	i_cdmus i_endoom i_joystick i_scale i_sound i_system i_timer memio \
	m_argv m_bbox m_cheat m_config m_controls m_fixed m_menu m_misc \
	m_random p_ceilng p_doors p_enemy p_floor p_inter p_lights p_map \
	p_maputl p_mobj p_plats p_pspr p_saveg p_setup p_sight p_spec \
	p_switch p_telept p_tick p_user r_bsp r_data r_draw r_main r_plane \
	r_segs r_sky r_things sha1 sounds statdump st_lib st_stuff s_sound \
	tables v_video wi_stuff w_checksum w_file w_main w_wad z_zone \
	w_file_stdc i_input i_video doomgeneric mus2mid

OBJS = $(addprefix $(OBJDIR)/,$(addsuffix .o,$(SRC_ENGINE))) \
       $(OBJDIR)/doomgeneric_revive.o

all: $(OUTPUT)

$(OUTPUT): $(OBJS)
	$(CC) $(CFLAGS) $(OBJS) -o $@ $(LIBS)

$(OBJDIR)/%.o: $(ENGINE_DIR)/%.c | $(OBJDIR)
	$(CC) $(CFLAGS) -c $< -o $@

$(OBJDIR)/doomgeneric_revive.o: $(REVIVE_DIR)/doomgeneric_revive.c | $(OBJDIR)
	$(CC) $(CFLAGS) -c $< -o $@

$(OBJDIR):
	mkdir -p $(OBJDIR)

# Compile with the host compiler, just to check that the code builds.
nativo:
	$(MAKE) CC="$${CC_NATIVE:-cc}" OBJDIR=build-native \
	        OUTPUT=revive-doom-native all

clean:
	rm -rf build build-native revive-doom revive-doom-native

.PHONY: all nativo clean
