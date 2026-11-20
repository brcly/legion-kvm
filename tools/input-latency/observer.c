/* Diagnostic only: private-session virtual keyboard -> independently captured
 * compositor pixels. No desktop input, guest clocks, screenshots or readbacks
 * are added to normal Legion launches or FPS measurements. */
#define _GNU_SOURCE
#include <wayland-client.h>
#include <xkbcommon/xkbcommon.h>
#include "wlr-screencopy-unstable-v1-client-protocol.h"
#include "virtual-keyboard-unstable-v1-client-protocol.h"
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <poll.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/inotify.h>
#include <sys/stat.h>
#include <sys/timerfd.h>
#include <time.h>
#include <unistd.h>

static struct wl_display *display;
static struct wl_shm *shm;
static struct wl_output *output;
static struct wl_seat *seat;
static struct zwlr_screencopy_manager_v1 *capture;
static struct zwp_virtual_keyboard_manager_v1 *keyboard_manager;
static struct zwp_virtual_keyboard_v1 *keyboard;
static struct zwlr_screencopy_frame_v1 *frame;
static struct wl_buffer *buffer;
static uint8_t *pixels;
static size_t extent;
static uint32_t width, height, stride, format;
static unsigned pixel_bytes;
static unsigned requested=40, samples, captures, invalid, sequence;
static bool ready, waiting, finished, no_input;
static int timer_fd;
static uint64_t began, request_ns, injection_ns, last_old_ns, timer_due;
static uint32_t random_state=0x59d1e67b;
static const char *failure;
static const char *feedback_path;
static int feedback_fd=-1, notify_fd=-1;
static bool runtime_ready, final_capture, final_capture_pending;
static unsigned final_capture_delay_ms, dispatch_delay_ms;
static uint64_t control_started_ns;
static uint64_t qpc_frequency, last_present, last_fence;
static unsigned timed_captures;


static uint64_t now(void)
{
    struct timespec ts;
    if(clock_gettime(CLOCK_MONOTONIC,&ts)) exit(1);
    return (uint64_t)ts.tv_sec*1000000000+ts.tv_nsec;
}
static void schedule(uint64_t ns)
{
    timer_due=ns;
    struct itimerspec timer={.it_value={ns/1000000000,ns%1000000000}};
    if(timerfd_settime(timer_fd,TFD_TIMER_ABSTIME,&timer,NULL)) {failure="set timer";finished=true;}
}
static uint64_t interval(void)
{
    random_state^=random_state<<13;random_state^=random_state>>17;random_state^=random_state<<5;
    return 80000000+(random_state%80000000); /* randomized phase, fixed seed */
}
static int decode(void)
{
    if(!pixels || width<64 || height!=1 || stride<width*pixel_bytes) return -1;
    uint32_t word=0;
    for(unsigned bit=0;bit<32;bit++) {
        unsigned center=(uint64_t)width*(bit*2+1)/64;
        int state=-1;
        for(int dx=-1;dx<=1;dx++) {
            const uint8_t *p=pixels+(center+dx)*pixel_bytes;
            /* Black/white codes remain robust to RGB/BGR and colour transforms.
             * Mid-tone, non-grey or conflicting pixels cannot identify a tag. */
            unsigned low=p[0]<p[1]?p[0]:p[1],high=p[0]>p[1]?p[0]:p[1];
            if(p[2]<low)low=p[2];if(p[2]>high)high=p[2];
            if(high-low>12 || (low<128 && high>80) || (low>=128 && low<180))return -1;
            int value=low>=128;
            if(state>=0 && value!=state)return -1;
            state=value;
        }
        word|=(uint32_t)state<<bit;
    }
    if((word>>16)!=0xa5c3)return -1;
    return word&65535;
}

struct feedback_record {
    unsigned sequence;
    uint64_t target,present,input,submitted,feedback,polled,fence,completed;
};
static bool parse_feedback(const char *line, struct feedback_record *r)
{
    int consumed=0;
    return sscanf(line,"LEGION_FEEDBACK %u %"SCNu64" %"SCNu64" %"SCNu64" %"SCNu64" %"SCNu64" %"SCNu64" %"SCNu64" %"SCNu64"%n",
        &r->sequence,&r->target,&r->present,&r->input,&r->submitted,&r->feedback,&r->polled,&r->fence,&r->completed,&consumed)==9 &&
        line[consumed]=='\0' && r->sequence>0 && r->sequence<=200 && r->target>0 && r->present>=r->target &&
        r->input>0 && r->submitted>=r->input && r->feedback>=r->input && r->polled>=r->submitted && r->polled>=r->feedback &&
        r->fence>0 && r->completed>=r->fence && r->completed!=UINT64_MAX;
}
static void begin_capture(void);
static void runtime_line(const char *line)
{
    uint64_t frequency=0;int consumed=0;
    if(sscanf(line,"LEGION_FEEDBACK_READY %"SCNu64"%n",&frequency,&consumed)==1 && line[consumed]=='\0') {
        if(runtime_ready || !frequency || frequency>1000000000000ULL) {failure="invalid runtime clock capability";finished=true;return;}
        runtime_ready=true;qpc_frequency=frequency;
        printf("{\"event\":\"runtime_ready\",\"qpc_frequency\":%"PRIu64"}\n",frequency);
        if(ready)schedule(now()+1000000000+interval());
        return;
    }
    unsigned readback_sequence;uint64_t readback_qpc;consumed=0;
    if(sscanf(line,"LEGION_FEEDBACK_READBACK %u %"SCNu64"%n",&readback_sequence,&readback_qpc,&consumed)==2 && line[consumed]=='\0') {
        if(injection_ns && samples<requested) {failure="source readback overlapped timed runtime phase";finished=true;}
        return;
    }
    struct feedback_record r;
    if(!runtime_ready || !waiting || !parse_feedback(line,&r) || r.sequence!=sequence+1 || r.target<=last_present || r.fence<=last_fence) {
        failure="invalid, stale or unsolicited runtime frame feedback";finished=true;return;
    }
    uint64_t observed=now();
    printf("{\"event\":\"sample\",\"sequence\":%u,\"injected_ns\":%"PRIu64",\"observed_ns\":%"PRIu64",\"response_ms\":%.6f,\"control_started_ns\":%"PRIu64",\"dispatch_delay_ns\":%"PRIu64",\"target_present_id\":%"PRIu64",\"feedback_present_id\":%"PRIu64",\"source_input_qpc\":%"PRIu64",\"source_submitted_qpc\":%"PRIu64",\"source_feedback_qpc\":%"PRIu64",\"source_polled_qpc\":%"PRIu64",\"fence_value\":%"PRIu64",\"completed_fence_value\":%"PRIu64",\"source_input_to_feedback_ms\":%.6f,\"source_feedback_to_poll_ms\":%.6f}\n",
        r.sequence,injection_ns,observed,(observed-injection_ns)/1e6,control_started_ns,injection_ns-control_started_ns,r.target,r.present,r.input,r.submitted,r.feedback,r.polled,r.fence,r.completed,
        (r.feedback-r.input)*1000.0/qpc_frequency,(r.polled-r.feedback)*1000.0/qpc_frequency);
    last_present=r.target;last_fence=r.fence;sequence=r.sequence;samples++;waiting=false;
    if(samples==requested) {
        final_capture=true;
        if(final_capture_delay_ms) {final_capture_pending=true;schedule(observed+(uint64_t)final_capture_delay_ms*1000000);}
        else {schedule(observed+5000000000ULL);begin_capture();}
    }
    else schedule(observed+interval());
}
static void runtime_read(void)
{
    static char bytes[4096];static size_t used;
    for(;;) {
        ssize_t count=read(feedback_fd,bytes+used,sizeof(bytes)-used-1);
        if(count<0) {if(errno==EINTR)continue;failure="read runtime feedback";finished=true;return;}
        if(!count)return;
        used+=(size_t)count;bytes[used]='\0';
        char *newline;
        while((newline=memchr(bytes,'\n',used))) {
            size_t len=(size_t)(newline-bytes);*newline='\0';runtime_line(bytes);
            memmove(bytes,bytes+len+1,used-len-1);used-=len+1;
            if(finished)return;
        }
        if(used==sizeof(bytes)-1) {failure="runtime record too long";finished=true;return;}
    }
}
static void runtime_open(void)
{
    if(!feedback_path || feedback_fd>=0)return;
    feedback_fd=open(feedback_path,O_RDONLY|O_CLOEXEC|O_NONBLOCK|O_NOFOLLOW);
    if(feedback_fd<0) {if(errno!=ENOENT){failure="open runtime feedback";finished=true;}return;}
    struct stat st;
    if(fstat(feedback_fd,&st) || !S_ISREG(st.st_mode) || st.st_uid!=getuid()) {failure="runtime feedback is not an owned file";finished=true;return;}
    notify_fd=inotify_init1(IN_CLOEXEC|IN_NONBLOCK);
    if(notify_fd<0 || inotify_add_watch(notify_fd,feedback_path,IN_MODIFY|IN_DELETE_SELF|IN_MOVE_SELF)<0) {failure="watch runtime feedback";finished=true;return;}
    runtime_read();
}


static void got_buffer(void *data,struct zwlr_screencopy_frame_v1 *f,uint32_t fmt,uint32_t w,uint32_t h,uint32_t pitch)
{
    (void)data;(void)f;
    unsigned bytes=(fmt==WL_SHM_FORMAT_BGR888 || fmt==WL_SHM_FORMAT_RGB888)?3:4;
    if((fmt!=WL_SHM_FORMAT_ARGB8888 && fmt!=WL_SHM_FORMAT_XRGB8888 &&
        fmt!=WL_SHM_FORMAT_ABGR8888 && fmt!=WL_SHM_FORMAT_XBGR8888 &&
        fmt!=WL_SHM_FORMAT_BGR888 && fmt!=WL_SHM_FORMAT_RGB888) || w!=1920 || h!=1 || pitch<w*bytes || pitch>16384) {
        fprintf(stderr,"capture format=%#x width=%u height=%u stride=%u\n",fmt,w,h,pitch);
        failure="unexpected capture geometry/format";finished=true;return;
    }
    if(buffer) {
        if(width!=w || height!=h || stride!=pitch || format!=fmt) {failure="capture format changed";finished=true;}
        return;
    }
    width=w;height=h;stride=pitch;format=fmt;pixel_bytes=bytes;extent=(size_t)stride*height;
    int fd=memfd_create("legion-latency-row",MFD_CLOEXEC);
    if(fd<0 || ftruncate(fd,(off_t)extent)) {failure="allocate capture row";finished=true;if(fd>=0)close(fd);return;}
    pixels=mmap(NULL,extent,PROT_READ|PROT_WRITE,MAP_SHARED,fd,0);
    if(pixels==MAP_FAILED) {pixels=NULL;failure="map capture row";finished=true;close(fd);return;}
    struct wl_shm_pool *pool=wl_shm_create_pool(shm,fd,(int32_t)extent);
    buffer=wl_shm_pool_create_buffer(pool,0,(int32_t)width,(int32_t)height,(int32_t)stride,format);
    wl_shm_pool_destroy(pool);close(fd);
}
static void got_done(void *data,struct zwlr_screencopy_frame_v1 *f)
{
    (void)data;
    if(!finished && buffer)zwlr_screencopy_frame_v1_copy(f,buffer);
}
static void got_ready(void *data,struct zwlr_screencopy_frame_v1 *f,uint32_t hi,uint32_t lo,uint32_t ns)
{
    (void)data;
    uint64_t observed=now(), timestamp=(((uint64_t)hi<<32)|lo)*1000000000+ns;
    int tag=decode();captures++;
    if(feedback_path && injection_ns>0 && !final_capture)timed_captures++;
    zwlr_screencopy_frame_v1_destroy(f);frame=NULL;
    if(final_capture) {
        if(tag==(int)requested) {
            printf("{\"event\":\"final_image_verified\",\"sequence\":%u,\"time_ns\":%"PRIu64",\"capture_requested_ns\":%"PRIu64"}\n",requested,observed,request_ns);
            finished=true;
        } else begin_capture();
        return;
    }
    if(tag<0)invalid++;
    else if(!ready) {
        if(tag!=0){failure="fixture sequence was not initially zero";finished=true;return;}
        ready=true;last_old_ns=observed;
        printf("{\"event\":\"fixture_ready\",\"magic\":42435,\"time_ns\":%"PRIu64"}\n",observed);
        if(!feedback_path || runtime_ready)schedule(observed+1000000000+interval());
    } else if(waiting && tag==(int)(sequence+1)) {
        samples++;sequence++;
        printf("{\"event\":\"sample\",\"sequence\":%u,\"injected_ns\":%"PRIu64",\"observed_ns\":%"PRIu64",\"capture_requested_ns\":%"PRIu64",\"capture_timestamp_ns\":%"PRIu64",\"last_old_observed_ns\":%"PRIu64",\"response_ms\":%.6f,\"capture_ms\":%.6f}\n",
            sequence,injection_ns,observed,request_ns,timestamp,last_old_ns,(observed-injection_ns)/1e6,(observed-request_ns)/1e6);
        waiting=false;last_old_ns=observed;
        if(samples==requested)finished=true;else schedule(observed+interval());
    } else if(tag==(int)sequence)last_old_ns=observed;
    else if(tag>=0) {failure="unexpected or skipped fixture sequence";finished=true;}
    if(!finished && (!feedback_path || !ready))begin_capture();
}
static void got_failed(void *data,struct zwlr_screencopy_frame_v1 *f)
{
    (void)data;zwlr_screencopy_frame_v1_destroy(f);frame=NULL;failure="compositor capture failed";finished=true;
}
static void got_flags(void *d,struct zwlr_screencopy_frame_v1 *f,uint32_t flags){(void)d;(void)f;(void)flags;}
static void got_damage(void *d,struct zwlr_screencopy_frame_v1 *f,uint32_t x,uint32_t y,uint32_t w,uint32_t h){(void)d;(void)f;(void)x;(void)y;(void)w;(void)h;}
static void got_dmabuf(void *d,struct zwlr_screencopy_frame_v1 *f,uint32_t fmt,uint32_t w,uint32_t h){(void)d;(void)f;(void)fmt;(void)w;(void)h;}
static const struct zwlr_screencopy_frame_v1_listener frame_listener={
    .buffer=got_buffer,.flags=got_flags,.ready=got_ready,.failed=got_failed,.damage=got_damage,
    .linux_dmabuf=got_dmabuf,.buffer_done=got_done,
};
static void begin_capture(void)
{
    request_ns=now();
    frame=zwlr_screencopy_manager_v1_capture_output_region(capture,0,output,0,540,1920,1);
    zwlr_screencopy_frame_v1_add_listener(frame,&frame_listener,NULL);
}
static void registry_global(void *data,struct wl_registry *registry,uint32_t name,const char *interface,uint32_t version)
{
    (void)data;
    if(!strcmp(interface,"wl_shm"))shm=wl_registry_bind(registry,name,&wl_shm_interface,1);
    if(!strcmp(interface,"wl_output") && !output)output=wl_registry_bind(registry,name,&wl_output_interface,1);
    if(!strcmp(interface,"wl_seat") && !seat)seat=wl_registry_bind(registry,name,&wl_seat_interface,1);
    if(!strcmp(interface,"zwlr_screencopy_manager_v1") && version>=3)capture=wl_registry_bind(registry,name,&zwlr_screencopy_manager_v1_interface,3);
    if(!strcmp(interface,"zwp_virtual_keyboard_manager_v1"))keyboard_manager=wl_registry_bind(registry,name,&zwp_virtual_keyboard_manager_v1_interface,1);
}
static void registry_remove(void *data,struct wl_registry *registry,uint32_t name){(void)data;(void)registry;(void)name;}
static const struct wl_registry_listener registry_listener={registry_global,registry_remove};

static int self_test(void)
{
    width=1920;height=1;
    const uint32_t ids[]={0,1,2,40,0x1234,65535};
    for(pixel_bytes=3;pixel_bytes<=4;pixel_bytes++) {
        stride=width*pixel_bytes;pixels=calloc(1,stride);if(!pixels)return 1;
        for(unsigned id=0;id<sizeof(ids)/sizeof(ids[0]);id++) {
            uint32_t word=0xa5c30000|ids[id];
            for(unsigned x=0;x<width;x++) {
                unsigned bit=(uint64_t)x*32/width;
                memset(pixels+x*pixel_bytes,(word&(1u<<bit))?230:20,3);
            }
            if(decode()!=(int)ids[id])return 1;
            unsigned center=(uint64_t)width*33/64;
            pixels[center*pixel_bytes]=128;
            if(decode()!=-1)return 1;
        }
        memset(pixels,0,stride);if(decode()!=-1)return 1;
        free(pixels);pixels=NULL;
    }
    struct feedback_record record;
    if(!parse_feedback("LEGION_FEEDBACK 1 7 8 100 110 120 130 9 9",&record))return 1;
    const char *bad[]={"LEGION_FEEDBACK 0 7 8 100 110 120 130 9 9",
        "LEGION_FEEDBACK 1 7 6 100 110 120 130 9 9", "LEGION_FEEDBACK 1 7 8 100 110 90 130 9 9",
        "LEGION_FEEDBACK 1 7 8 100 110 120 130 9 8", "LEGION_FEEDBACK 1 7 8 100 110 120 130 9 9 junk"};
    for(unsigned i=0;i<sizeof(bad)/sizeof(bad[0]);i++)if(parse_feedback(bad[i],&record))return 1;
    puts("observer parser tests passed: 24/32-bit pixels, sequence, corruption and placeholder rejection");
    return 0;
}

int main(int argc,char **argv)
{
    setvbuf(stdout,NULL,_IOLBF,0);
    if(argc==2 && !strcmp(argv[1],"--self-test"))return self_test();
    for(int i=1;i<argc;i++) {
        if(!strcmp(argv[i],"--samples") && i+1<argc)requested=(unsigned)atoi(argv[++i]);
        else if(!strcmp(argv[i],"--runtime-feedback") && i+1<argc)feedback_path=argv[++i];
        else if(!strcmp(argv[i],"--dispatch-delay-ms") && i+1<argc)dispatch_delay_ms=(unsigned)atoi(argv[++i]);
        else if(!strcmp(argv[i],"--final-capture-delay-ms") && i+1<argc)final_capture_delay_ms=(unsigned)atoi(argv[++i]);
        else if(!strcmp(argv[i],"--no-input"))no_input=true;
        else {fputs("unknown observer option\n",stderr);return 1;}
    }
    if(requested<1 || requested>200 || dispatch_delay_ms>100 || final_capture_delay_ms>1000 || (final_capture_delay_ms && !feedback_path))return 1;
    const char *runtime=getenv("XDG_RUNTIME_DIR"),*socket=getenv("WAYLAND_DISPLAY");
    struct stat st;
    if(!runtime || !socket || strchr(socket,'/') || strncmp(runtime,"/tmp/legion-latency-",20) ||
       stat(runtime,&st) || st.st_uid!=getuid() || (st.st_mode&077)!=0) {
        fputs("observer requires a private legion-latency runtime directory\n",stderr);return 1;
    }
    display=wl_display_connect(NULL);if(!display)return 1;
    struct wl_registry *registry=wl_display_get_registry(display);
    wl_registry_add_listener(registry,&registry_listener,NULL);
    if(wl_display_roundtrip(display)<0 || !shm || !output || !seat || !capture || !keyboard_manager) {
        fputs("private compositor lacks capture or virtual keyboard\n",stderr);return 1;
    }
    struct xkb_context *xkb=xkb_context_new(XKB_CONTEXT_NO_FLAGS);
    struct xkb_rule_names names={.layout="us"};
    struct xkb_keymap *keymap=xkb_keymap_new_from_names(xkb,&names,XKB_KEYMAP_COMPILE_NO_FLAGS);
    if(!keymap)return 1;
    char *text=xkb_keymap_get_as_string(keymap,XKB_KEYMAP_FORMAT_TEXT_V1);
    int fd=memfd_create("legion-latency-keymap",MFD_CLOEXEC);size_t len=strlen(text)+1;
    if(fd<0 || write(fd,text,len)!=(ssize_t)len)return 1;
    keyboard=zwp_virtual_keyboard_manager_v1_create_virtual_keyboard(keyboard_manager,seat);
    zwp_virtual_keyboard_v1_keymap(keyboard,WL_KEYBOARD_KEYMAP_FORMAT_XKB_V1,fd,(uint32_t)len);
    zwp_virtual_keyboard_v1_modifiers(keyboard,0,0,0,0);
    close(fd);free(text);xkb_keymap_unref(keymap);xkb_context_unref(xkb);
    timer_fd=timerfd_create(CLOCK_MONOTONIC,TFD_CLOEXEC|TFD_NONBLOCK);
    if(timer_fd<0)return 1;
    began=now();schedule(began+90000000000ULL);begin_capture();
    while(!finished) {
        runtime_open();
        if(finished)break;
        while(wl_display_prepare_read(display)!=0) {
            if(wl_display_dispatch_pending(display)<0){failure="Wayland dispatch";finished=true;break;}
        }
        if(finished)break;
        if(wl_display_flush(display)<0 && errno!=EAGAIN){wl_display_cancel_read(display);failure="Wayland flush";break;}
        struct pollfd fds[]={{wl_display_get_fd(display),POLLIN,0},{timer_fd,POLLIN,0},{notify_fd,POLLIN,0}};
        int ret=poll(fds,3,feedback_path && feedback_fd<0?10:1000);
        if(ret<0 && errno==EINTR){wl_display_cancel_read(display);continue;}
        if(ret<0){wl_display_cancel_read(display);failure="poll failed";break;}
        if(fds[0].revents&POLLIN) {
            if(wl_display_read_events(display)<0){failure="Wayland disconnected";break;}
            if(wl_display_dispatch_pending(display)<0){failure="Wayland dispatch";break;}
        } else wl_display_cancel_read(display);
        if(fds[0].revents&(POLLERR|POLLHUP)){failure="Wayland connection lost";break;}
        if(notify_fd>=0 && (fds[2].revents&POLLIN)) {
            _Alignas(struct inotify_event) char events[4096];ssize_t count;
            while((count=read(notify_fd,events,sizeof(events)))>0) {
                for(size_t at=0;at<(size_t)count;) {
                    struct inotify_event *event=(struct inotify_event *)(void *)(events+at);
                    if(event->mask&(IN_DELETE_SELF|IN_MOVE_SELF)){failure="runtime feedback file was replaced";finished=true;}
                    at+=sizeof(*event)+event->len;
                }
            }
            if(!finished)runtime_read();
        }
        if(finished)break;
        if(fds[1].revents&POLLIN) {
            uint64_t expirations;if(read(timer_fd,&expirations,sizeof(expirations))<0)continue;
            if(!ready || (feedback_path && !runtime_ready)){failure="no owned fixture barcode or runtime capability";break;}
            if(final_capture_pending){final_capture_pending=false;schedule(now()+5000000000ULL);begin_capture();continue;}
            if(final_capture){failure="final runtime image verification timed out";break;}
            if(waiting){failure="input response timed out";break;}
            control_started_ns=now();
            if(dispatch_delay_ms) {
                struct timespec delay={.tv_nsec=(long)dispatch_delay_ms*1000000};
                int result;
                do {result=nanosleep(&delay,&delay);} while(result && errno==EINTR);
                if(result){failure="host dispatch calibration wait failed";break;}
            }
            injection_ns=now();waiting=true;
            if(!no_input) {
                zwp_virtual_keyboard_v1_key(keyboard,(uint32_t)(injection_ns/1000000),57,WL_KEYBOARD_KEY_STATE_PRESSED);
                zwp_virtual_keyboard_v1_key(keyboard,(uint32_t)(injection_ns/1000000),57,WL_KEYBOARD_KEY_STATE_RELEASED);
                wl_display_flush(display);
            }
            schedule(injection_ns+1000000000);
        }
    }
    if(feedback_path)printf("{\"event\":\"runtime_measurement\",\"timed_captures\":%u,\"scope\":\"host virtual-keyboard dispatch to received runtime frame-feedback record; includes transport and polling; not host display or photons\"}\n",timed_captures);
    printf("{\"event\":\"summary\",\"schema\":1,\"passed\":%s,\"samples\":%u,\"requested\":%u,\"captures\":%u,\"invalid_captures\":%u,\"last_sequence\":%u,\"no_input_control\":%s,\"error\":%s%s%s,\"scope\":\"%s\"}\n",
        !failure && samples==requested?"true":"false",samples,requested,captures,invalid,sequence,no_input?"true":"false",failure?"\"":"",failure?failure:"null",failure?"\"":"",feedback_path?"host virtual-keyboard dispatch to received runtime frame-feedback record, not host display or photons":"private-session dispatch to independently captured response, not photons");
    if(feedback_fd>=0)close(feedback_fd);if(notify_fd>=0)close(notify_fd);
    if(frame)zwlr_screencopy_frame_v1_destroy(frame);
    if(buffer)wl_buffer_destroy(buffer);if(pixels)munmap(pixels,extent);
    close(timer_fd);wl_display_disconnect(display);
    return failure || samples!=requested;
}
