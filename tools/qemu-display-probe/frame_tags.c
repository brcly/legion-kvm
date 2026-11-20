/* Explicit diagnostic: GPU decodes fixture barcodes into an eight-byte PBO.
 * CPU access follows the viewer's original GL fence, covering this source read
 * too. Disabled paths allocate nothing and perform no extra GPU operations. */
#include "frame_tags.h"
#include <epoxy/gl.h>
#include <stdio.h>
#include <string.h>
static GLuint program, framebuffer, image, pbo;
static const char *vertex=
    "#version 330 core\nvoid main(){vec2 p=vec2((gl_VertexID<<1)&2,gl_VertexID&2);gl_Position=vec4(p*2.-1.,0.,1.);}";
static const char *fragment=
    "#version 330 core\nuniform sampler2D source;out vec4 color;"
    "uint decode(int y){ivec2 size=textureSize(source,0);uint word=0u;"
    "for(int bit=0;bit<32;bit++){int x=size.x*(bit*2+1)/64;int state=-1;"
    "for(int dx=-1;dx<=1;dx++){vec3 p=texelFetch(source,ivec2(x+dx,y),0).rgb*255.;"
    "float lo=min(p.r,min(p.g,p.b)),hi=max(p.r,max(p.g,p.b));"
    "if(hi-lo>12.||(lo<128.&&hi>80.)||(lo>=128.&&lo<180.))return 0u;"
    "int value=lo>=128.?1:0;if(state>=0&&value!=state)return 0u;state=value;}"
    "word|=uint(state)<<uint(bit);}return word;}"
    "void main(){ivec2 size=textureSize(source,0);uint word=0u;"
    "if(size.x>=64&&size.y>=32){if(gl_FragCoord.x<1.)word=decode(size.y/2);"
    "else{word=decode(4);if((word>>16)!=0xb7d1u)word=decode(size.y-5);}}"
    "color=vec4(word&255u,(word>>8)&255u,(word>>16)&255u,(word>>24)&255u)/255.;}";
static GLuint compile(GLenum type,const char *source)
{
    GLuint shader=glCreateShader(type);glShaderSource(shader,1,&source,NULL);glCompileShader(shader);
    GLint ok=0;glGetShaderiv(shader,GL_COMPILE_STATUS,&ok);
    if(!ok){char log[2048];glGetShaderInfoLog(shader,sizeof(log),NULL,log);fprintf(stderr,"frame tag shader: %s\n",log);glDeleteShader(shader);return 0;}
    return shader;
}
int frame_tags_open(void)
{
    GLuint vs=compile(GL_VERTEX_SHADER,vertex),ps=compile(GL_FRAGMENT_SHADER,fragment);
    if(!vs||!ps){if(vs)glDeleteShader(vs);if(ps)glDeleteShader(ps);return -1;}
    program=glCreateProgram();glAttachShader(program,vs);glAttachShader(program,ps);glLinkProgram(program);
    glDeleteShader(vs);glDeleteShader(ps);GLint ok=0;glGetProgramiv(program,GL_LINK_STATUS,&ok);if(!ok)return -1;
    glGenTextures(1,&image);glBindTexture(GL_TEXTURE_2D,image);
    glTexImage2D(GL_TEXTURE_2D,0,GL_RGBA8,2,1,0,GL_RGBA,GL_UNSIGNED_BYTE,NULL);
    glGenFramebuffers(1,&framebuffer);glBindFramebuffer(GL_FRAMEBUFFER,framebuffer);
    glFramebufferTexture2D(GL_FRAMEBUFFER,GL_COLOR_ATTACHMENT0,GL_TEXTURE_2D,image,0);
    ok=glCheckFramebufferStatus(GL_FRAMEBUFFER)==GL_FRAMEBUFFER_COMPLETE;
    glBindFramebuffer(GL_FRAMEBUFFER,0);
    glGenBuffers(1,&pbo);glBindBuffer(GL_PIXEL_PACK_BUFFER,pbo);glBufferData(GL_PIXEL_PACK_BUFFER,8,NULL,GL_STREAM_READ);glBindBuffer(GL_PIXEL_PACK_BUFFER,0);
    return ok&&glGetError()==GL_NO_ERROR?0:-1;
}
int frame_tags_record(unsigned texture)
{
    if(!program)return 0;
    GLboolean dither=glIsEnabled(GL_DITHER),srgb=glIsEnabled(GL_FRAMEBUFFER_SRGB);
    glDisable(GL_DITHER);glDisable(GL_FRAMEBUFFER_SRGB);
    glBindFramebuffer(GL_FRAMEBUFFER,framebuffer);glViewport(0,0,2,1);
    glUseProgram(program);glUniform1i(glGetUniformLocation(program,"source"),0);
    glBindTexture(GL_TEXTURE_2D,texture);glDrawArrays(GL_TRIANGLES,0,3);
    glBindBuffer(GL_PIXEL_PACK_BUFFER,pbo);glReadPixels(0,0,2,1,GL_RGBA,GL_UNSIGNED_BYTE,NULL);
    glBindBuffer(GL_PIXEL_PACK_BUFFER,0);glBindFramebuffer(GL_FRAMEBUFFER,0);
    if(dither)glEnable(GL_DITHER);
    if(srgb)glEnable(GL_FRAMEBUFFER_SRGB);
    return glGetError()==GL_NO_ERROR?0:-1;
}
int frame_tags_read(struct frame_tag *tag)
{
    *tag=(struct frame_tag){0};if(!program)return 0;
    glBindBuffer(GL_PIXEL_PACK_BUFFER,pbo);
    const unsigned char *bytes=glMapBufferRange(GL_PIXEL_PACK_BUFFER,0,8,GL_MAP_READ_BIT);
    if(!bytes){glBindBuffer(GL_PIXEL_PACK_BUFFER,0);return -1;}
    uint32_t words[2]={0};
    for(unsigned i=0;i<8;i++)words[i/4]|=(uint32_t)bytes[i]<<(8*(i%4));
    GLboolean ok=glUnmapBuffer(GL_PIXEL_PACK_BUFFER);glBindBuffer(GL_PIXEL_PACK_BUFFER,0);
    if(!ok||glGetError()!=GL_NO_ERROR)return -1;
    if((words[0]>>16)==0xa5c3u&&(words[1]>>16)==0xb7d1u&&(words[1]&65535u))
        *tag=(struct frame_tag){words[0]&65535u,words[1]&65535u,1};
    return 0;
}
void frame_tags_close(void)
{
    if(pbo)glDeleteBuffers(1,&pbo);
    if(framebuffer)glDeleteFramebuffers(1,&framebuffer);
    if(image)glDeleteTextures(1,&image);
    if(program)glDeleteProgram(program);
    pbo=framebuffer=image=program=0;
}
