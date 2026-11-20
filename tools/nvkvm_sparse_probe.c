/* Owned Vulkan sparse-buffer correctness probe; run unchanged on host and guest. */
#include <vulkan/vulkan.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <stdarg.h>
#include <errno.h>
#include <stdatomic.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

static _Atomic int tracing;
static _Atomic int trace_initial_queries;
int ioctl(int fd, unsigned long request, ...)
{
    va_list arguments;
    va_start(arguments, request);
    void *pointer=va_arg(arguments, void *);
    va_end(arguments);
    int result=(int)syscall(SYS_ioctl,fd,request,pointer);
    int saved=errno;
    if (atomic_load(&trace_initial_queries) && pointer && request == 0xc020462aUL) {
        uint32_t inner, status;
        memcpy(&inner, (const char *)pointer+8, sizeof(inner));
        memcpy(&status, (const char *)pointer+28, sizeof(status));
        if (inner == 0x20802a0cu || inner == 0x2080019fu)
            fprintf(stderr,"initial RM query=0x%08x ioctl_result=%d nvstatus=0x%08x\n",inner,result,status);
    }
    if (atomic_load(&tracing)) {
        fprintf(stderr,"ioctl fd=%d cmd=0x%lx result=%d errno=%d data=",fd,request,result,result<0?saved:0);
        size_t bytes=(request>>16)&0x3fff;
        if (bytes>128) bytes=128;
        if (pointer) for (size_t i=0;i<bytes;i++) fprintf(stderr,"%02x",((unsigned char *)pointer)[i]);
        fputc('\n',stderr);
    }
    errno=saved;
    return result;
}

#define CHECK(call) do { VkResult r = (call); if (r != VK_SUCCESS) { \
    fprintf(stderr, "{\"passed\":false,\"api\":\"%s\",\"result\":%d}\n", #call, r); return 1; } } while (0)

static uint32_t memory_type(VkPhysicalDevice gpu, uint32_t mask, VkMemoryPropertyFlags flags)
{
    VkPhysicalDeviceMemoryProperties memory;
    vkGetPhysicalDeviceMemoryProperties(gpu, &memory);
    for (uint32_t i = 0; i < memory.memoryTypeCount; i++)
        if ((mask & (1u << i)) && (memory.memoryTypes[i].propertyFlags & flags) == flags) return i;
    fputs("required memory type unavailable\n", stderr);
    exit(1);
}

int main(void)
{
    atomic_store(&trace_initial_queries,getenv("LEGION_DRIVER_INIT_TRACE") != NULL);
    VkInstance instance;
    VkInstanceCreateInfo ici = { .sType=VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO };
    CHECK(vkCreateInstance(&ici, NULL, &instance));
    uint32_t count=1;
    VkPhysicalDevice gpu;
    CHECK(vkEnumeratePhysicalDevices(instance, &count, &gpu));
    VkPhysicalDeviceProperties properties;
    vkGetPhysicalDeviceProperties(gpu, &properties);
    printf("device=%s\n", properties.deviceName);
    VkPhysicalDeviceFeatures supported;
    vkGetPhysicalDeviceFeatures(gpu, &supported);
    if (!supported.sparseBinding || !supported.sparseResidencyBuffer) return 77;
    uint32_t families=0, family=UINT32_MAX;
    vkGetPhysicalDeviceQueueFamilyProperties(gpu, &families, NULL);
    VkQueueFamilyProperties *queues=calloc(families, sizeof(*queues));
    if (!queues) return 1;
    vkGetPhysicalDeviceQueueFamilyProperties(gpu, &families, queues);
    for (uint32_t i=0; i<families; i++)
        if ((queues[i].queueFlags & (VK_QUEUE_GRAPHICS_BIT|VK_QUEUE_SPARSE_BINDING_BIT)) ==
            (VK_QUEUE_GRAPHICS_BIT|VK_QUEUE_SPARSE_BINDING_BIT)) { family=i; break; }
    free(queues);
    if (family==UINT32_MAX) return 77;
    float priority=1.0f;
    VkDeviceQueueCreateInfo qci={ .sType=VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO, .queueFamilyIndex=family,
        .queueCount=1, .pQueuePriorities=&priority };
    VkPhysicalDeviceFeatures features={ .sparseBinding=VK_TRUE, .sparseResidencyBuffer=VK_TRUE };
    VkDeviceCreateInfo dci={ .sType=VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO, .queueCreateInfoCount=1,
        .pQueueCreateInfos=&qci, .pEnabledFeatures=&features };
    VkDevice device;
    CHECK(vkCreateDevice(gpu,&dci,NULL,&device));
    VkQueue queue;
    vkGetDeviceQueue(device,family,0,&queue);
    VkBufferCreateInfo bci={ .sType=VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size=1024*1024,
        .usage=VK_BUFFER_USAGE_TRANSFER_SRC_BIT|VK_BUFFER_USAGE_TRANSFER_DST_BIT,
        .flags=VK_BUFFER_CREATE_SPARSE_BINDING_BIT|VK_BUFFER_CREATE_SPARSE_RESIDENCY_BIT };
    VkBuffer buffer, readback;
    CHECK(vkCreateBuffer(device,&bci,NULL,&buffer));
    VkMemoryRequirements req;
    vkGetBufferMemoryRequirements(device,buffer,&req);
    VkMemoryAllocateInfo mai={ .sType=VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize=req.size*2,
        .memoryTypeIndex=memory_type(gpu,req.memoryTypeBits,VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT) };
    VkDeviceMemory memory, host;
    CHECK(vkAllocateMemory(device,&mai,NULL,&memory));
    bci.flags=0;
    CHECK(vkCreateBuffer(device,&bci,NULL,&readback));
    VkMemoryRequirements host_req;
    vkGetBufferMemoryRequirements(device,readback,&host_req);
    mai.allocationSize=host_req.size;
    mai.memoryTypeIndex=memory_type(gpu,host_req.memoryTypeBits,VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT|VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
    CHECK(vkAllocateMemory(device,&mai,NULL,&host));
    CHECK(vkBindBufferMemory(device,readback,host,0));
    VkFenceCreateInfo fci={ .sType=VK_STRUCTURE_TYPE_FENCE_CREATE_INFO };
    VkFence fence;
    CHECK(vkCreateFence(device,&fci,NULL,&fence));
    VkCommandPoolCreateInfo pci={ .sType=VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
        .flags=VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT, .queueFamilyIndex=family };
    VkCommandPool pool;
    CHECK(vkCreateCommandPool(device,&pci,NULL,&pool));
    VkCommandBufferAllocateInfo cai={ .sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
        .commandPool=pool, .level=VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount=1 };
    VkCommandBuffer command;
    CHECK(vkAllocateCommandBuffers(device,&cai,&command));
    for (unsigned round=0; round<4; round++) {
        VkSparseMemoryBind bind={ .size=req.size, .memory=memory, .memoryOffset=(round%2)*req.size };
        VkSparseBufferMemoryBindInfo bi={ .buffer=buffer, .bindCount=1, .pBinds=&bind };
        VkBindSparseInfo si={ .sType=VK_STRUCTURE_TYPE_BIND_SPARSE_INFO, .bufferBindCount=1, .pBufferBinds=&bi };
        printf("round=%u queue bind\n",round); fflush(stdout);
        atomic_store(&tracing,getenv("LEGION_SPARSE_TRACE") != NULL);
        CHECK(vkQueueBindSparse(queue,1,&si,fence));
        CHECK(vkWaitForFences(device,1,&fence,VK_TRUE,5000000000ULL));
        CHECK(vkResetFences(device,1,&fence));
        atomic_store(&tracing,0);
        CHECK(vkResetCommandBuffer(command,0));
        VkCommandBufferBeginInfo begin={ .sType=VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO };
        CHECK(vkBeginCommandBuffer(command,&begin));
        uint32_t value=0x13579bdfu+round;
        vkCmdFillBuffer(command,buffer,0,bci.size,value);
        VkMemoryBarrier barrier={ .sType=VK_STRUCTURE_TYPE_MEMORY_BARRIER,
            .srcAccessMask=VK_ACCESS_TRANSFER_WRITE_BIT, .dstAccessMask=VK_ACCESS_TRANSFER_READ_BIT };
        vkCmdPipelineBarrier(command,VK_PIPELINE_STAGE_TRANSFER_BIT,VK_PIPELINE_STAGE_TRANSFER_BIT,0,1,&barrier,0,NULL,0,NULL);
        VkBufferCopy copy={ .size=bci.size };
        vkCmdCopyBuffer(command,buffer,readback,1,&copy);
        barrier.srcAccessMask=VK_ACCESS_TRANSFER_WRITE_BIT;
        barrier.dstAccessMask=VK_ACCESS_HOST_READ_BIT;
        vkCmdPipelineBarrier(command,VK_PIPELINE_STAGE_TRANSFER_BIT,VK_PIPELINE_STAGE_HOST_BIT,
                             0,1,&barrier,0,NULL,0,NULL);
        CHECK(vkEndCommandBuffer(command));
        VkSubmitInfo submit={ .sType=VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount=1, .pCommandBuffers=&command };
        CHECK(vkQueueSubmit(queue,1,&submit,fence));
        CHECK(vkWaitForFences(device,1,&fence,VK_TRUE,5000000000ULL));
        CHECK(vkResetFences(device,1,&fence));
        uint32_t *pixels;
        CHECK(vkMapMemory(device,host,0,bci.size,0,(void **)&pixels));
        for (size_t i=0;i<bci.size/4;i++) if (pixels[i]!=value) { fprintf(stderr,"wrong pixel at %zu\n",i); return 1; }
        vkUnmapMemory(device,host);
    }
    CHECK(vkDeviceWaitIdle(device));
    vkDestroyCommandPool(device,pool,NULL); vkDestroyFence(device,fence,NULL);
    vkDestroyBuffer(device,readback,NULL); vkFreeMemory(device,host,NULL);
    vkDestroyBuffer(device,buffer,NULL); vkFreeMemory(device,memory,NULL);
    vkDestroyDevice(device,NULL); vkDestroyInstance(instance,NULL);
    puts("{\"passed\":true,\"scope\":\"sparse buffer bind/rebind and GPU contents\"}");
    return 0;
}
