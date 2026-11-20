/* CPU-only proxy tests: real provider is loaded globally before the proxy,
 * matching the difficult SDL/GLVND lookup order without creating GPU work. */
#define _GNU_SOURCE
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <stdint.h>
#include <string.h>
#ifdef MOCK_PROVIDER
static EGLint assigned,forced;
static unsigned created,destroyed;
static EGLint saved[131];
void test_force(EGLint value) { forced=value; }
unsigned test_count(int key) { return key?destroyed:created; }
EGLint *test_attributes(void) { return saved; }
EGLContext eglCreateContext(EGLDisplay d,EGLConfig c,EGLContext s,const EGLint *a)
{
    (void)d;(void)c;(void)s;unsigned i=0;assigned=EGL_CONTEXT_PRIORITY_MEDIUM_IMG;
    while(a && a[i]!=EGL_NONE && i<128) {saved[i]=a[i];saved[i+1]=a[i+1];if(a[i]==EGL_CONTEXT_PRIORITY_LEVEL_IMG)assigned=a[i+1];i+=2;}
    saved[i]=EGL_NONE;if(forced)assigned=forced;created++;return (EGLContext)(uintptr_t)1;
}
EGLBoolean eglQueryContext(EGLDisplay d,EGLContext c,EGLint key,EGLint *value)
{(void)d;(void)c;(void)key;*value=assigned;return EGL_TRUE;}
EGLBoolean eglDestroyContext(EGLDisplay d,EGLContext c)
{(void)d;(void)c;destroyed++;return EGL_TRUE;}
__eglMustCastToProperFunctionPointerType eglGetProcAddress(const char *name)
{return !strcmp(name,"eglCreateContext")?(__eglMustCastToProperFunctionPointerType)eglCreateContext:NULL;}
#else
#include <assert.h>
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
int main(int argc,char **argv)
{
    assert(argc==2);
    void *provider=dlopen("libEGL.so.1",RTLD_NOW|RTLD_GLOBAL);assert(provider);
    void *proxy=dlopen(argv[1],RTLD_NOW|RTLD_LOCAL);assert(proxy);
    PFNEGLCREATECONTEXTPROC create=dlsym(proxy,"eglCreateContext");assert(create);
    PFNEGLGETPROCADDRESSPROC proc=dlsym(proxy,"eglGetProcAddress");assert(proc);
    PFNEGLCREATECONTEXTPROC indirect=(PFNEGLCREATECONTEXTPROC)proc("eglCreateContext");assert(indirect);
    void (*force)(EGLint)=dlsym(provider,"test_force");
    unsigned (*count)(int)=dlsym(provider,"test_count");
    EGLint *(*attributes)(void)=dlsym(provider,"test_attributes");assert(force&&count&&attributes);
    EGLint original[]={EGL_CONTEXT_CLIENT_VERSION,3,EGL_CONTEXT_PRIORITY_LEVEL_IMG,EGL_CONTEXT_PRIORITY_MEDIUM_IMG,EGL_NONE};
    EGLint before[5];memcpy(before,original,sizeof(original));
    setenv("LEGION_VIEWER_GPU_PRIORITY","high",1);
    assert(indirect(NULL,NULL,NULL,original)!=EGL_NO_CONTEXT);
    assert(!memcmp(before,original,sizeof(original)));
    assert(attributes()[0]==EGL_CONTEXT_CLIENT_VERSION&&attributes()[1]==3);
    assert(attributes()[2]==EGL_CONTEXT_PRIORITY_LEVEL_IMG&&attributes()[3]==EGL_CONTEXT_PRIORITY_HIGH_IMG&&attributes()[4]==EGL_NONE);
    assert(create(NULL,NULL,NULL,NULL)!=EGL_NO_CONTEXT);
    assert(attributes()[0]==EGL_CONTEXT_PRIORITY_LEVEL_IMG&&attributes()[1]==EGL_CONTEXT_PRIORITY_HIGH_IMG&&attributes()[2]==EGL_NONE);
    force(EGL_CONTEXT_PRIORITY_MEDIUM_IMG);unsigned destroyed=count(1);
    assert(create(NULL,NULL,NULL,NULL)==EGL_NO_CONTEXT&&count(1)==destroyed+1);force(0);
    unsetenv("LEGION_VIEWER_GPU_PRIORITY");setenv("LEGION_COMPOSITOR_GPU_PRIORITY","high",1);unsetenv("XDG_RUNTIME_DIR");
    unsigned calls=count(0);assert(create(NULL,NULL,NULL,NULL)==EGL_NO_CONTEXT&&count(0)==calls);
    char directory[]="/tmp/legion-latency-priority-XXXXXX";assert(mkdtemp(directory));
    setenv("XDG_RUNTIME_DIR",directory,1);setenv("WLR_BACKENDS","headless",1);
    assert(create(NULL,NULL,NULL,NULL)!=EGL_NO_CONTEXT);assert(!rmdir(directory));
    puts("priority proxy tests passed: local/global lookup, get-proc dispatch, attributes, downgrade, private guard");
    return 0;
}
#endif
