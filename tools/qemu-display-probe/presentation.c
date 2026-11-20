/* Opt-in host-stage timing. Feedback belongs to a viewer update, not a Proton
 * frame ID. The protocol trace must independently audit swap/commit association.
 * No source pixels are mapped/read and GPU-read acknowledgements do not wait
 * for presentation callbacks. All calls run on the SDL context owner thread. */
#define _GNU_SOURCE
#include "presentation.h"
#include <SDL_syswm.h>
#include <wayland-client.h>
#include "presentation-time-client-protocol.h"
#include <fcntl.h>
#include <inttypes.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#define MAX_RECORDS 32768u
#define MAX_PENDING 128u
struct record {
    uint64_t frame, draw, gpu, swap, returned, received, presented, sequence;
    uint64_t device, inode, gpu_elapsed;
    uint32_t width, height;
    struct frame_tag tag;
    uint32_t refresh, flags, object;
    unsigned early;
    unsigned status; /* 0 pending; 1 presented; 2 discarded; 3 abandoned */
    struct wp_presentation_feedback *feedback;
};
static struct wl_display *connection;
static struct wl_surface *surface;
static struct wl_registry *registry;
static struct wl_event_queue *queue;
static struct wp_presentation *presentation;
static struct record *records, *swapping, *awaiting_read;
static struct presentation_stats counts;
static unsigned used, pending, delay_ms;
static bool enabled, clock_known, finalized;
static FILE *report;

uint64_t presentation_now(void)
{
    struct timespec value;
    if (clock_gettime(CLOCK_MONOTONIC, &value)) return 0;
    return (uint64_t)value.tv_sec * 1000000000u + (uint64_t)value.tv_nsec;
}
static void clock_id(void *data, struct wp_presentation *object, uint32_t id)
{
    (void)data; (void)object;
    counts.clock_id = id; clock_known = true;
    if (id != CLOCK_MONOTONIC) counts.errors++;
}
static const struct wp_presentation_listener presentation_listener = { .clock_id = clock_id };
static void global(void *data, struct wl_registry *object, uint32_t name, const char *interface, uint32_t version)
{
    (void)data;
    if (!presentation && version >= 1 && !strcmp(interface, "wp_presentation")) {
        presentation = wl_registry_bind(object, name, &wp_presentation_interface, 1);
        wp_presentation_add_listener(presentation, &presentation_listener, NULL);
    }
}
static void removed(void *data, struct wl_registry *object, uint32_t name)
{ (void)data; (void)object; (void)name; }
static const struct wl_registry_listener registry_listener = { global, removed };
static void output(void *data, struct wp_presentation_feedback *object, struct wl_output *out)
{ (void)data; (void)object; (void)out; }
static void complete(struct record *r, struct wp_presentation_feedback *object, unsigned status)
{
    r->received = presentation_now(); r->status = status;
    if (!r->returned || !r->gpu || r->received < r->returned || r->received < r->gpu || !pending) counts.errors++;
    if (pending) pending--;
    wp_presentation_feedback_destroy(object); r->feedback = NULL;
}
static void presented(void *data, struct wp_presentation_feedback *object,
                      uint32_t hi, uint32_t lo, uint32_t ns, uint32_t refresh,
                      uint32_t seq_hi, uint32_t seq_lo, uint32_t flags)
{
    struct record *r = data;
    uint64_t seconds = ((uint64_t)hi << 32) | lo;
    if (ns >= 1000000000u || seconds > (UINT64_MAX - ns) / 1000000000u) {
        counts.errors++; r->presented = 0;
    } else r->presented = seconds * 1000000000u + ns;
    r->sequence = ((uint64_t)seq_hi << 32) | seq_lo;
    r->refresh = refresh; r->flags = flags;
    complete(r, object, 1); counts.presented++;
    /* Report unusable clock/order rather than inventing a negative duration. */
    if (r->presented < (r->early?r->swap:r->gpu) || r->presented > r->received) counts.errors++;
}
static void discarded(void *data, struct wp_presentation_feedback *object)
{
    complete(data, object, 2); counts.discarded++;
}
static const struct wp_presentation_feedback_listener feedback_listener = {
    .sync_output = output, .presented = presented, .discarded = discarded,
};

int presentation_active(void) { return enabled; }

int presentation_open(SDL_Window *window, const char *path, unsigned delay)
{
    struct SDL_SysWMinfo info = {0};
    SDL_VERSION(&info.version);
    if (!SDL_GetWindowWMInfo(window, &info) || info.subsystem != SDL_SYSWM_WAYLAND || delay > 100) return -1;
    connection = info.info.wl.display; surface = info.info.wl.surface;
    delay_ms = delay;
    queue = wl_display_create_queue(connection);
    if (!queue) return -1;
    /* EGL may read this connection on another thread. Make the registry on
     * our queue atomically, rather than moving it after replies can arrive. */
    struct wl_display *wrapper = wl_proxy_create_wrapper(connection);
    if (!wrapper) return -1;
    wl_proxy_set_queue((struct wl_proxy *)wrapper, queue);
    registry = wl_display_get_registry(wrapper);
    wl_proxy_wrapper_destroy(wrapper);
    if (!registry) return -1;
    wl_registry_add_listener(registry, &registry_listener, NULL);
    if (wl_display_roundtrip_queue(connection, queue) < 0 ||
        wl_display_roundtrip_queue(connection, queue) < 0 || !presentation || !clock_known || counts.errors) return -1;
    int fd = open(path, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0600);
    if (fd < 0) return -1;
    report = fdopen(fd, "w");
    if (!report) { close(fd); return -1; }
    records = calloc(MAX_RECORDS, sizeof(*records));
    if (!records) return -1;
    enabled = true;
    fprintf(stderr, "LEGION_PRESENTATION_READY surface=%u clock_id=%"PRIu64" delay_ms=%u\n",
            wl_proxy_get_id((struct wl_proxy *)surface), counts.clock_id, delay_ms);
    return 0;
}
void presentation_begin(uint64_t frame, uint64_t draw_ns, uint64_t gpu_ns,
                        uint64_t device, uint64_t inode, uint32_t width, uint32_t height, const struct frame_tag *tag, uint64_t gpu_elapsed, int early)
{
    if (!enabled) return;
    counts.submitted++;
    if (counts.errors || used == MAX_RECORDS || pending >= MAX_PENDING || swapping || awaiting_read) { counts.errors++; return; }
    if (delay_ms) SDL_Delay(delay_ms); /* Positive control, after real GPU read completion. */
    struct record *r = &records[used++];
    r->early=early;
    r->tag = *tag; r->gpu_elapsed=gpu_elapsed;
    r->frame = frame; r->draw = draw_ns; r->gpu = gpu_ns;
    r->device = device; r->inode = inode; r->width=width; r->height=height; r->swap = presentation_now();
    if (!draw_ns || (early?(gpu_ns!=0 || delay_ms!=0 || r->swap<draw_ns):(gpu_ns < draw_ns || r->swap < gpu_ns))) counts.errors++;
    fprintf(stderr, "LEGION_SWAP_BEGIN frame=%"PRIu64" surface=%u\n", frame,
            wl_proxy_get_id((struct wl_proxy *)surface));
    r->feedback = wp_presentation_feedback(presentation, surface);
    if (!r->feedback) { counts.errors++; r->status = 3; counts.abandoned++; return; }
    r->object = wl_proxy_get_id((struct wl_proxy *)r->feedback);
    wp_presentation_feedback_add_listener(r->feedback, &feedback_listener, r);
    pending++; swapping = r;
}
void presentation_end(void)
{
    if (!enabled || !swapping) return;
    swapping->returned = presentation_now();
    fprintf(stderr, "LEGION_SWAP_END frame=%"PRIu64" surface=%u\n", swapping->frame,
            wl_proxy_get_id((struct wl_proxy *)surface));
    if(swapping->early) awaiting_read=swapping;
    swapping = NULL;
}
void presentation_read_complete(uint64_t gpu_ns, const struct frame_tag *tag, uint64_t gpu_elapsed)
{
    if(!enabled) return;
    if(!awaiting_read || !gpu_ns || gpu_ns<awaiting_read->returned) {counts.errors++;return;}
    awaiting_read->gpu=gpu_ns;awaiting_read->tag=*tag;awaiting_read->gpu_elapsed=gpu_elapsed;
    awaiting_read=NULL;
}
void presentation_poll(void)
{
    if (enabled && wl_display_dispatch_queue_pending(connection, queue) < 0) counts.errors++;
}
int presentation_finish(struct presentation_stats *out)
{
    if (!enabled) { *out = counts; return 0; }
    if (!finalized) {
        uint64_t deadline = presentation_now() + 250000000u;
        while (pending && presentation_now() < deadline) {
            SDL_PumpEvents(); presentation_poll(); SDL_Delay(1);
        }
        if (pending) {
            /* Explicitly unmap after the normal drain. Unshown tail updates
             * must be discarded, never called presented just to finish a run. */
            presentation_unmap();
            deadline=presentation_now()+250000000u;
            while (pending && presentation_now()<deadline) { SDL_PumpEvents(); presentation_poll(); SDL_Delay(1); }
        }
        fprintf(report, "{\"event\":\"configuration\",\"clock_id\":%"PRIu64",\"early_swap\":%u,\"delay_ms\":%u,\"surface\":%u,\"scope\":\"viewer update to host compositor presentation feedback; not source input or photons\"}\n",
                counts.clock_id, used?records[0].early:0, delay_ms, wl_proxy_get_id((struct wl_proxy *)surface));
        for (unsigned i = 0; i < used; i++) {
            struct record *r = &records[i];
            if (r->feedback) { wp_presentation_feedback_destroy(r->feedback); r->feedback = NULL; r->status = 3; counts.abandoned++; }
            fprintf(report, "{\"event\":\"frame\",\"frame\":%"PRIu64",\"feedback_object\":%u,\"status\":%u,\"draw_ns\":%"PRIu64",\"gpu_done_ns\":%"PRIu64",\"swap_begin_ns\":%"PRIu64",\"swap_return_ns\":%"PRIu64",\"received_ns\":%"PRIu64",\"presented_ns\":%"PRIu64",\"refresh_ns\":%u,\"sequence\":%"PRIu64",\"flags\":%u,\"early_swap\":%u,\"gpu_elapsed_ns\":%"PRIu64",\"tag_valid\":%u,\"input_generation\":%u,\"source_frame_id\":%u,\"source_width\":%u,\"source_height\":%u,\"dmabuf_device\":%"PRIu64",\"dmabuf_inode\":%"PRIu64"}\n",
                    r->frame, r->object, r->status, r->draw, r->gpu, r->swap, r->returned,
                    r->received, r->presented, r->refresh, r->sequence, r->flags, r->early, r->gpu_elapsed, r->tag.valid, r->tag.generation, r->tag.source_frame, r->width, r->height, r->device, r->inode);
        }
        pending = 0;
        if (fclose(report)) counts.errors++;
        report = NULL; finalized = true;
    }
    *out = counts;
    return counts.errors || counts.abandoned || !counts.presented || counts.submitted != counts.presented + counts.discarded ? -1 : 0;
}
void presentation_close(void)
{
    if (records) for (unsigned i = 0; i < used; i++) if (records[i].feedback) wp_presentation_feedback_destroy(records[i].feedback);
    free(records); records = NULL;
    if (report) fclose(report);
    report = NULL;
    if (presentation) wp_presentation_destroy(presentation);
    if (registry) wl_registry_destroy(registry);
    if (queue) wl_event_queue_destroy(queue);
    presentation = NULL; registry = NULL; queue = NULL;
    enabled = false;
    /* The SDL display/surface are borrowed, never destroyed here. */
}
