/* Host-only private-compositor experiment. Loaded only into the owned headless
 * Sway process, or selected as the owned viewer's SDL EGL driver proxy. */
#define _GNU_SOURCE
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <dlfcn.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

typedef EGLContext (*create_fn)(EGLDisplay,EGLConfig,EGLContext,const EGLint *);
static create_fn real_create;
static __eglMustCastToProperFunctionPointerType (*real_get_proc)(const char *);
static pthread_once_t initialized=PTHREAD_ONCE_INIT;
static void initialize(void)
{
    /* SDL loads this proxy locally, so RTLD_NEXT need not include libEGL if
     * that library was already loaded. Resolve against its exact handle. */
    void *provider=dlopen("libEGL.so.1",RTLD_NOW|RTLD_LOCAL);
    if (provider) {
        real_create=(create_fn)dlsym(provider,"eglCreateContext");
        real_get_proc=dlsym(provider,"eglGetProcAddress");
    }
}

static EGLContext create_context(EGLDisplay display,EGLConfig config,EGLContext share,const EGLint *attributes)
{
    pthread_once(&initialized,initialize);
    if (!real_create) { fputs("LEGION_COMPOSITOR_GPU_PRIORITY error=missing-real-create\n",stderr);return EGL_NO_CONTEXT; }
    const char *viewer=getenv("LEGION_VIEWER_GPU_PRIORITY");
    const char *setting=viewer?viewer:getenv("LEGION_COMPOSITOR_GPU_PRIORITY");
    const char *role=viewer?"VIEWER":"COMPOSITOR";
    if (!viewer) {
        const char *runtime=getenv("XDG_RUNTIME_DIR"),*backend=getenv("WLR_BACKENDS");
        struct stat st;
        if (!runtime || strncmp(runtime,"/tmp/legion-latency-",20) || stat(runtime,&st) ||
            st.st_uid!=getuid() || (st.st_mode&077) || !S_ISDIR(st.st_mode) ||
            !backend || strcmp(backend,"headless")) {
            fputs("LEGION_COMPOSITOR_GPU_PRIORITY error=requires-owned-private-headless-compositor\n",stderr);
            return EGL_NO_CONTEXT;
        }
    }
    if (!setting) return EGL_NO_CONTEXT;
    EGLint requested=0;
    if (!strcmp(setting,"medium")) requested=EGL_CONTEXT_PRIORITY_MEDIUM_IMG;
    else if (!strcmp(setting,"high")) requested=EGL_CONTEXT_PRIORITY_HIGH_IMG;
    else if (strcmp(setting,"observe")) return EGL_NO_CONTEXT;
    EGLint copy[131];unsigned used=0;int found=0;
    while (attributes && attributes[used]!=EGL_NONE) {
        if (used>=128) { fputs("LEGION_COMPOSITOR_GPU_PRIORITY error=attribute-bound\n",stderr);return EGL_NO_CONTEXT; }
        copy[used]=attributes[used];copy[used+1]=attributes[used+1];
        if (copy[used]==EGL_CONTEXT_PRIORITY_LEVEL_IMG) { found=1;if(requested)copy[used+1]=requested; }
        used+=2;
    }
    if (requested && !found) { copy[used++]=EGL_CONTEXT_PRIORITY_LEVEL_IMG;copy[used++]=requested; }
    copy[used]=EGL_NONE;
    EGLContext context=real_create(display,config,share,requested?copy:attributes);
    EGLint assigned=0;
    if (context!=EGL_NO_CONTEXT && !eglQueryContext(display,context,EGL_CONTEXT_PRIORITY_LEVEL_IMG,&assigned)) assigned=0;
    fprintf(stderr,"LEGION_%s_GPU_PRIORITY requested=%#x assigned=%#x valid=%u method=original-context\n",role,requested,assigned,context!=EGL_NO_CONTEXT);
    if (requested && assigned!=requested && context!=EGL_NO_CONTEXT) {
        eglDestroyContext(display,context);return EGL_NO_CONTEXT;
    }
    return context;
}

EGLContext eglCreateContext(EGLDisplay display,EGLConfig config,EGLContext share,const EGLint *attributes)
{ return create_context(display,config,share,attributes); }

__eglMustCastToProperFunctionPointerType eglGetProcAddress(const char *name)
{
    pthread_once(&initialized,initialize);
    if (!strcmp(name,"eglCreateContext")) return (__eglMustCastToProperFunctionPointerType)create_context;
    return real_get_proc?real_get_proc(name):NULL;
}
