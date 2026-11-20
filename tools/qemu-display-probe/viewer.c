/* Research-only host DMA-BUF viewer. All SDL/EGL/GL calls stay on its owning
 * thread. Normal rendering never maps or reads frame pixels through the CPU.
 * Explicit tag diagnostics read back eight decoded bytes per frame. An update is
 * acknowledged only after a real GL fence covers the final source read. */
#include <SDL.h>
#include <SDL_syswm.h>
#include "presentation.h"
#include "frame_tags.h"
#include "stage_profile.h"
#include <epoxy/egl.h>
#include <epoxy/gl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/resource.h>

struct viewer_event { int32_t kind, a, b; };
struct image { uint64_t inode, device; EGLImageKHR image; GLuint texture; uint32_t w,h,fourcc,offset,stride; uint64_t modifier; };
static SDL_Window *window;
static SDL_GLContext context;
static EGLDisplay display;
static GLuint program, vao, gpu_queries[2];
static struct image images[8];
static unsigned next_image, current_image;
static int flipped, grabbed, early_swap;
static uint32_t frame_w, frame_h;
static uint64_t fence_wait_ns=1000000;

int viewer_fence_wait(uint32_t microseconds)
{
    if(microseconds<50 || microseconds>1000) return -1;
    fence_wait_ns=(uint64_t)microseconds*1000;
    return 0;
}

uint64_t viewer_cpu_ns(void)
{
    struct rusage usage;
    if(getrusage(RUSAGE_SELF,&usage)) return UINT64_MAX;
    return ((uint64_t)usage.ru_utime.tv_sec+(uint64_t)usage.ru_stime.tv_sec)*1000000000
        +((uint64_t)usage.ru_utime.tv_usec+(uint64_t)usage.ru_stime.tv_usec)*1000;
}

static GLuint shader(GLenum type, const char *source)
{
    GLuint s = glCreateShader(type);
    glShaderSource(s, 1, &source, NULL);
    glCompileShader(s);
    GLint ok = 0;
    glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
    if (!ok) { char text[2048]; glGetShaderInfoLog(s, sizeof(text), NULL, text); fprintf(stderr,"shader: %s\n",text); glDeleteShader(s); return 0; }
    return s;
}

int viewer_gpu_priority(void)
{
    EGLint value=0;
    if (epoxy_has_egl_extension(display,"EGL_IMG_context_priority"))
        eglQueryContext(display,eglGetCurrentContext(),EGL_CONTEXT_PRIORITY_LEVEL_IMG,&value);
    return value;
}

int viewer_open(const char *title, int fullscreen, int gpu_priority)
{
    SDL_SetHint(SDL_HINT_VIDEO_X11_FORCE_EGL, "1");
    SDL_SetHint(SDL_HINT_GRAB_KEYBOARD, "1");
    if (SDL_Init(SDL_INIT_VIDEO)) goto fail;
    SDL_GL_SetAttribute(SDL_GL_CONTEXT_EGL, 1);
    SDL_GL_SetAttribute(SDL_GL_CONTEXT_MAJOR_VERSION, 3);
    SDL_GL_SetAttribute(SDL_GL_CONTEXT_MINOR_VERSION, 3);
    SDL_GL_SetAttribute(SDL_GL_CONTEXT_PROFILE_MASK, SDL_GL_CONTEXT_PROFILE_CORE);
    window = SDL_CreateWindow(title, SDL_WINDOWPOS_UNDEFINED, SDL_WINDOWPOS_UNDEFINED,
        1920, 1080, SDL_WINDOW_OPENGL | SDL_WINDOW_RESIZABLE | (fullscreen ? SDL_WINDOW_FULLSCREEN_DESKTOP : 0));
    if (!window) goto fail;
    context = SDL_GL_CreateContext(window);
    if (!context) goto fail;
    display = eglGetCurrentDisplay();
    if (display == EGL_NO_DISPLAY || !epoxy_has_egl_extension(display,"EGL_EXT_image_dma_buf_import_modifiers")) goto fail;
    if (gpu_priority && viewer_gpu_priority()!=gpu_priority) {
        fprintf(stderr,"viewer GPU priority was not granted on SDL context: requested=%#x actual=%#x\n",gpu_priority,viewer_gpu_priority());
        goto fail;
    }
    if (SDL_GL_SetSwapInterval(0)) goto fail;
    GLuint vertex = shader(GL_VERTEX_SHADER,
        "#version 330 core\nout vec2 uv; void main(){uv=vec2((gl_VertexID<<1)&2,gl_VertexID&2); gl_Position=vec4(uv*2.-1.,0.,1.);}");
    GLuint fragment = shader(GL_FRAGMENT_SHADER,
        "#version 330 core\nin vec2 uv; out vec4 color; uniform sampler2D image; uniform int flipped; void main(){color=texture(image,vec2(uv.x,flipped!=0?1.-uv.y:uv.y));}");
    if (!vertex || !fragment) goto fail;
    program = glCreateProgram();
    glAttachShader(program,vertex); glAttachShader(program,fragment); glLinkProgram(program);
    glDeleteShader(vertex); glDeleteShader(fragment);
    GLint ok=0; glGetProgramiv(program,GL_LINK_STATUS,&ok);
    if (!ok) goto fail;
    glUseProgram(program);
    glUniform1i(glGetUniformLocation(program,"image"),0);
    glGenVertexArrays(1,&vao); glBindVertexArray(vao);
    fprintf(stderr,"viewer: renderer=%s swap_interval=%d frame_transport=dmabuf completion=gl-fence\n",glGetString(GL_RENDERER),SDL_GL_GetSwapInterval());
    return 0;
fail:
    fprintf(stderr,"viewer initialization failed: SDL=%s EGL=%x GL=%x\n",SDL_GetError(),eglGetError(),context?glGetError():0);
    return -1;
}

int viewer_import(const int32_t *fds, const uint32_t *offsets, const uint32_t *strides,
                  uint32_t planes, uint32_t width, uint32_t height,
                  uint32_t backing_width, uint32_t backing_height,
                  uint32_t x, uint32_t y, uint32_t fourcc, uint64_t modifier, int y0_top)
{
    /* The first prototype deliberately refuses geometry it cannot represent. */
    if (planes != 1 || !width || !height || width > 8192 || height > 8192 ||
        x || y || width != backing_width || height != backing_height || !strides[0]) return -1;
    struct stat st;
    if (fstat(fds[0],&st)) return -1;
    for (unsigned i=0;i<8;i++) {
        struct image *a=&images[i];
        if (a->texture && a->inode==(uint64_t)st.st_ino && a->device==(uint64_t)st.st_dev && a->w==width && a->h==height && a->fourcc==fourcc && a->modifier==modifier && a->offset==offsets[0] && a->stride==strides[0]) {
            current_image=i; frame_w=width; frame_h=height; flipped=!y0_top; return 0;
        }
    }
    unsigned slot=next_image++%8;
    struct image *a=&images[slot];
    if (a->texture) { glDeleteTextures(1,&a->texture); eglDestroyImageKHR(display,a->image); memset(a,0,sizeof(*a)); }
    EGLint attrs[]={ EGL_WIDTH,(EGLint)width,EGL_HEIGHT,(EGLint)height,
        EGL_LINUX_DRM_FOURCC_EXT,(EGLint)fourcc,EGL_DMA_BUF_PLANE0_FD_EXT,fds[0],
        EGL_DMA_BUF_PLANE0_OFFSET_EXT,(EGLint)offsets[0],EGL_DMA_BUF_PLANE0_PITCH_EXT,(EGLint)strides[0],
        EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT,(EGLint)modifier,EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT,(EGLint)(modifier>>32),EGL_NONE };
    a->image=eglCreateImageKHR(display,EGL_NO_CONTEXT,EGL_LINUX_DMA_BUF_EXT,NULL,attrs);
    if (a->image==EGL_NO_IMAGE_KHR) { fprintf(stderr,"DMA-BUF import failed EGL=%x\n",eglGetError()); return -1; }
    glGenTextures(1,&a->texture); glBindTexture(GL_TEXTURE_2D,a->texture);
    glEGLImageTargetTexture2DOES(GL_TEXTURE_2D,a->image);
    glTexParameteri(GL_TEXTURE_2D,GL_TEXTURE_MIN_FILTER,GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D,GL_TEXTURE_MAG_FILTER,GL_NEAREST);
    if (glGetError()!=GL_NO_ERROR) return -1;
    a->inode=(uint64_t)st.st_ino; a->device=(uint64_t)st.st_dev; a->w=width; a->h=height; a->fourcc=fourcc; a->modifier=modifier; a->offset=offsets[0]; a->stride=strides[0];
    current_image=slot; frame_w=width; frame_h=height; flipped=!y0_top;
    return 0;
}

int viewer_gpu_timing_open(void)
{
    GLint bits=0;glGetQueryiv(GL_TIMESTAMP,GL_QUERY_COUNTER_BITS,&bits);
    if(bits<32) return -1;
    glGenQueries(2,gpu_queries);
    return glGetError()==GL_NO_ERROR?0:-1;
}

int viewer_early_swap_open(void)
{
    struct SDL_SysWMinfo info={0};SDL_VERSION(&info.version);
    if(!SDL_GetWindowWMInfo(window,&info) || info.subsystem!=SDL_SYSWM_WAYLAND || !presentation_active()) return -1;
    early_swap=1;return 0;
}

int viewer_draw(uint64_t frame_id, int fail_after_swap)
{
    if (!images[current_image].texture) return -1;
    struct stage_record *stage=NULL;
    if (stage_profile_enabled()) {
        stage=stage_profile_next(frame_id);
        if (!stage) return -1;
        stage->begin=presentation_now();
    }
    uint64_t draw_ns = presentation_active() ? presentation_now() : 0;
    int width,height; SDL_GL_GetDrawableSize(window,&width,&height);
    glViewport(0,0,width,height);
    glUseProgram(program); glBindVertexArray(vao);
    glBindTexture(GL_TEXTURE_2D,images[current_image].texture);
    glUniform1i(glGetUniformLocation(program,"flipped"),flipped);
    if(gpu_queries[0]) glQueryCounter(gpu_queries[0],GL_TIMESTAMP);
    if(stage) stage->state=presentation_now();
    glDrawArrays(GL_TRIANGLES,0,3);
    if(stage) stage->draw=presentation_now();
    if (frame_tags_record(images[current_image].texture)) return -1;
    if(stage) stage->tag=presentation_now();
    if(gpu_queries[0]) glQueryCounter(gpu_queries[1],GL_TIMESTAMP);
    GLsync fence=glFenceSync(GL_SYNC_GPU_COMMANDS_COMPLETE,0);
    if (!fence || glGetError()!=GL_NO_ERROR) return -1;
    if(stage) stage->fence=presentation_now();
    glFlush();
    if(stage) stage->flushed=presentation_now();
    if(early_swap) {
        struct frame_tag empty={0};
        presentation_begin(frame_id,draw_ns,0,images[current_image].device,images[current_image].inode,frame_w,frame_h,&empty,0,1);
        SDL_GL_SwapWindow(window);presentation_end();
        if(fail_after_swap) {
            fprintf(stderr,"LEGION_VIEWER_INJECTED_READ_FAILURE after_swap=1 successful_reply=0\n");
            glDeleteSync(fence);return -1;
        }
    }
    /* Bound failure handling. Never acknowledge a timeout or failed wait. */
    uint64_t deadline=SDL_GetTicks64()+2000;
    GLenum status;
    if(stage) stage->wait_begin=presentation_now();
    do {
        status=glClientWaitSync(fence,0,fence_wait_ns);
        if(stage) {
            if(!stage->waits) stage->first_status=status;
            stage->waits++;
            stage->timeouts+=(status==GL_TIMEOUT_EXPIRED);
        }
    }
    while (status==GL_TIMEOUT_EXPIRED && SDL_GetTicks64()<deadline);
    if(stage) {stage->wait_end=presentation_now();stage->final_status=status;}
    glDeleteSync(fence);
    if (status!=GL_ALREADY_SIGNALED && status!=GL_CONDITION_SATISFIED) return -1;
    uint64_t gpu_ns = presentation_active() ? presentation_now() : 0;
    uint64_t gpu_elapsed=0;
    if(gpu_queries[0]) {
        GLint ready[2]={0};GLuint64 ticks[2]={0};
        for(unsigned i=0;i<2;i++) glGetQueryObjectiv(gpu_queries[i],GL_QUERY_RESULT_AVAILABLE,&ready[i]);
        if(!ready[0] || !ready[1]) return -1;
        for(unsigned i=0;i<2;i++) glGetQueryObjectui64v(gpu_queries[i],GL_QUERY_RESULT,&ticks[i]);
        if(ticks[1]<=ticks[0] || glGetError()!=GL_NO_ERROR) return -1;
        gpu_elapsed=ticks[1]-ticks[0];
    }
    struct frame_tag tag;
    if (frame_tags_read(&tag)) return -1;
    if(stage) {stage->post_wait=presentation_now();stage->gpu_elapsed=gpu_elapsed;}
    if(early_swap) presentation_read_complete(gpu_ns,&tag,gpu_elapsed);
    else {
        presentation_begin(frame_id,draw_ns,gpu_ns,images[current_image].device,images[current_image].inode,frame_w,frame_h,&tag,gpu_elapsed,0);
        if(stage) stage->swap_begin=presentation_now();
        SDL_GL_SwapWindow(window);
        if(stage) stage->swap_end=presentation_now();
        presentation_end();
    }
    return 0;
}

int viewer_event(struct viewer_event *out)
{
    presentation_poll();
    SDL_Event e;
    while (SDL_PollEvent(&e)) {
        *out=(struct viewer_event){0};
        if (e.type==SDL_QUIT) out->kind=1;
        if (e.type==SDL_WINDOWEVENT && e.window.event==SDL_WINDOWEVENT_FOCUS_LOST) { out->kind=7; grabbed=0; SDL_SetRelativeMouseMode(SDL_FALSE); SDL_SetWindowKeyboardGrab(window,SDL_FALSE); }
        if (e.type==SDL_KEYDOWN || e.type==SDL_KEYUP) {
            if (e.key.repeat) continue;
            if (e.key.keysym.scancode==SDL_SCANCODE_G && (e.key.keysym.mod&KMOD_CTRL) && (e.key.keysym.mod&KMOD_ALT)) {
                if (e.type==SDL_KEYDOWN) { grabbed=!grabbed; SDL_SetRelativeMouseMode(grabbed); SDL_SetWindowKeyboardGrab(window,grabbed); *out=(struct viewer_event){8,grabbed,0}; return 1; }
                continue;
            }
            out->kind=2; out->a=e.key.keysym.scancode; out->b=e.type==SDL_KEYDOWN;
        }
        if (e.type==SDL_MOUSEMOTION) {
            if (!frame_w || !frame_h) continue;
            out->kind=grabbed?4:3;
            int w,h; SDL_GetWindowSize(window,&w,&h);
            out->a=grabbed?e.motion.xrel:(int64_t)e.motion.x*frame_w/(w?w:1);
            out->b=grabbed?e.motion.yrel:(int64_t)e.motion.y*frame_h/(h?h:1);
            if (!grabbed) {
                if(out->a<0)out->a=0;
                if(out->b<0)out->b=0;
                if((uint32_t)out->a>=frame_w)out->a=(int32_t)frame_w-1;
                if((uint32_t)out->b>=frame_h)out->b=(int32_t)frame_h-1;
            }
        }
        if (e.type==SDL_MOUSEBUTTONDOWN || e.type==SDL_MOUSEBUTTONUP) { out->kind=5; out->a=e.button.button; out->b=e.type==SDL_MOUSEBUTTONDOWN; }
        if (e.type==SDL_MOUSEWHEEL) { out->kind=6; out->a=e.wheel.y; out->b=e.wheel.x; }
        if (out->kind) return 1;
    }
    presentation_poll();
    return 0;
}

int viewer_presentation_open(const char *path, uint32_t delay_ms)
{ return presentation_open(window, path, delay_ms); }

void viewer_close(void)
{
    stage_profile_close();
    presentation_close();
    frame_tags_close();
    if(gpu_queries[0]) glDeleteQueries(2,gpu_queries);
    for (unsigned i=0;i<8;i++) if(images[i].texture) {glDeleteTextures(1,&images[i].texture);eglDestroyImageKHR(display,images[i].image);}
    if (context) {glDeleteProgram(program);glDeleteVertexArrays(1,&vao);}
    if (context) SDL_GL_DeleteContext(context);
    if (window) SDL_DestroyWindow(window);
    SDL_Quit();
}

void presentation_unmap(void) { if (window) SDL_HideWindow(window); }
