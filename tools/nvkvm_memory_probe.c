/* Owned CPU-mapped Vulkan memory diagnostic. Run the identical binary on both
 * sides; volatile scalar accesses avoid differing host/guest SIMD policies.
 * This measures CPU mappings, not game FPS or GPU memory bandwidth. */
#include <vulkan/vulkan.h>
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>

#define CHECK(call) do { VkResult r = (call); if (r != VK_SUCCESS) { \
    fprintf(stderr, "%s failed: %d\n", #call, r); exit(1); } } while (0)

static double now(void)
{
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC_RAW, &ts)) exit(1);
    return ts.tv_sec + ts.tv_nsec * 1e-9;
}

static uint64_t word(size_t i, unsigned pass)
{
    return UINT64_C(0xcbe4a703195fd268) ^ (uint64_t)i ^ ((uint64_t)pass << 32);
}

static uint32_t memory_type(const VkPhysicalDeviceMemoryProperties *memory,
                            uint32_t bits, VkMemoryPropertyFlags required,
                            VkMemoryPropertyFlags excluded)
{
    for (uint32_t i = 0; i < memory->memoryTypeCount; ++i) {
        VkMemoryPropertyFlags flags = memory->memoryTypes[i].propertyFlags;
        if ((bits & (1u << i)) && (flags & required) == required && !(flags & excluded))
            return i;
    }
    fputs("required memory type unavailable\n", stderr);
    exit(1);
}

int main(void)
{
    const VkDeviceSize bytes = 32 * 1024 * 1024;
    const size_t words = (size_t)bytes / sizeof(uint64_t);
    const size_t read_words = 256 * 1024 / sizeof(uint64_t);
    const unsigned passes = 8;
    VkApplicationInfo application = { .sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
        .pApplicationName = "legion-mapped-memory", .apiVersion = VK_API_VERSION_1_1 };
    VkInstanceCreateInfo ici = { .sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
        .pApplicationInfo = &application };
    VkInstance instance;
    CHECK(vkCreateInstance(&ici, NULL, &instance));
    uint32_t count = 0;
    CHECK(vkEnumeratePhysicalDevices(instance, &count, NULL));
    if (count != 1) { fputs("select exactly one Vulkan GPU\n", stderr); return 1; }
    VkPhysicalDevice gpu;
    CHECK(vkEnumeratePhysicalDevices(instance, &count, &gpu));
    VkPhysicalDeviceProperties properties;
    VkPhysicalDeviceMemoryProperties memory;
    vkGetPhysicalDeviceProperties(gpu, &properties);
    vkGetPhysicalDeviceMemoryProperties(gpu, &memory);
    uint32_t families = 0, family = UINT32_MAX;
    vkGetPhysicalDeviceQueueFamilyProperties(gpu, &families, NULL);
    VkQueueFamilyProperties *queues = calloc(families, sizeof(*queues));
    if (!queues) return 1;
    vkGetPhysicalDeviceQueueFamilyProperties(gpu, &families, queues);
    for (uint32_t i = 0; i < families; ++i)
        if (queues[i].queueFlags & VK_QUEUE_GRAPHICS_BIT) { family = i; break; }
    free(queues);
    if (family == UINT32_MAX) return 1;
    float priority = 1;
    VkDeviceQueueCreateInfo qci = { .sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
        .queueFamilyIndex = family, .queueCount = 1, .pQueuePriorities = &priority };
    VkDeviceCreateInfo dci = { .sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
        .queueCreateInfoCount = 1, .pQueueCreateInfos = &qci };
    VkDevice device;
    CHECK(vkCreateDevice(gpu, &dci, NULL, &device));
    VkQueue queue;
    vkGetDeviceQueue(device, family, 0, &queue);
    VkCommandPoolCreateInfo pci = { .sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
        .flags = VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT, .queueFamilyIndex = family };
    VkCommandPool pool;
    CHECK(vkCreateCommandPool(device, &pci, NULL, &pool));
    VkCommandBufferAllocateInfo cai = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
        .commandPool = pool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount = 1 };
    VkCommandBuffer command;
    CHECK(vkAllocateCommandBuffers(device, &cai, &command));
    VkFenceCreateInfo fci = { .sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO };
    VkFence fence;
    CHECK(vkCreateFence(device, &fci, NULL, &fence));
    printf("{\"schema\":1,\"vendor_id\":%u,\"device_id\":%u,"
           "\"scope\":\"scalar CPU mapped-memory bandwidth; GPU verifies edge writes\",\"cases\":[",
           properties.vendorID, properties.deviceID);
    for (unsigned mode = 0; mode < 2; ++mode) {
        VkBufferCreateInfo bci = { .sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
            .size = bytes, .usage = VK_BUFFER_USAGE_TRANSFER_SRC_BIT };
        VkBuffer source, target;
        VkDeviceMemory source_memory, target_memory;
        CHECK(vkCreateBuffer(device, &bci, NULL, &source));
        VkMemoryRequirements req;
        vkGetBufferMemoryRequirements(device, source, &req);
        VkMemoryPropertyFlags coherent = VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT |
                                         VK_MEMORY_PROPERTY_HOST_COHERENT_BIT;
        uint32_t type = memory_type(&memory, req.memoryTypeBits,
            coherent | (mode ? VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT : VK_MEMORY_PROPERTY_HOST_CACHED_BIT),
            mode ? 0 : VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
        VkMemoryAllocateInfo mai = { .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
            .allocationSize = req.size, .memoryTypeIndex = type };
        CHECK(vkAllocateMemory(device, &mai, NULL, &source_memory));
        CHECK(vkBindBufferMemory(device, source, source_memory, 0));
        void *mapped;
        CHECK(vkMapMemory(device, source_memory, 0, bytes, 0, &mapped));
        volatile uint64_t *data = mapped;
        for (size_t i = 0; i < words; ++i) data[i] = word(i, 0); /* Fault/warm up. */
        __asm__ volatile("mfence" ::: "memory");
        double start = now();
        for (unsigned pass = 1; pass <= passes; ++pass) {
            for (size_t i = 0; i < words; ++i) data[i] = word(i, pass);
            __asm__ volatile("mfence" ::: "memory");
        }
        double write_seconds = now() - start;
        uint64_t sum = 0, expected = 0;
        for (size_t i = 0; i < read_words; ++i) expected += word(i, passes);
        start = now();
        for (size_t i = 0; i < read_words; ++i) sum += data[i];
        double read_seconds = now() - start;
        if (sum != expected) { fputs("CPU read mismatch\n", stderr); return 1; }
        bci.size = 128;
        bci.usage = VK_BUFFER_USAGE_TRANSFER_DST_BIT;
        CHECK(vkCreateBuffer(device, &bci, NULL, &target));
        vkGetBufferMemoryRequirements(device, target, &req);
        mai.allocationSize = req.size;
        mai.memoryTypeIndex = memory_type(&memory, req.memoryTypeBits,
            coherent | VK_MEMORY_PROPERTY_HOST_CACHED_BIT, 0);
        CHECK(vkAllocateMemory(device, &mai, NULL, &target_memory));
        CHECK(vkBindBufferMemory(device, target, target_memory, 0));
        void *readback;
        CHECK(vkMapMemory(device, target_memory, 0, 128, 0, &readback));
        VkCommandBufferBeginInfo begin = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO };
        CHECK(vkBeginCommandBuffer(command, &begin));
        VkMemoryBarrier barrier = { .sType = VK_STRUCTURE_TYPE_MEMORY_BARRIER,
            .srcAccessMask = VK_ACCESS_HOST_WRITE_BIT, .dstAccessMask = VK_ACCESS_TRANSFER_READ_BIT };
        vkCmdPipelineBarrier(command, VK_PIPELINE_STAGE_HOST_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT,
            0, 1, &barrier, 0, NULL, 0, NULL);
        VkBufferCopy copies[] = { { .size = 64 },
            { .srcOffset = bytes - 64, .dstOffset = 64, .size = 64 } };
        vkCmdCopyBuffer(command, source, target, 2, copies);
        barrier.srcAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT;
        barrier.dstAccessMask = VK_ACCESS_HOST_READ_BIT;
        vkCmdPipelineBarrier(command, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_HOST_BIT,
            0, 1, &barrier, 0, NULL, 0, NULL);
        CHECK(vkEndCommandBuffer(command));
        VkSubmitInfo submit = { .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO,
            .commandBufferCount = 1, .pCommandBuffers = &command };
        CHECK(vkQueueSubmit(queue, 1, &submit, fence));
        CHECK(vkWaitForFences(device, 1, &fence, VK_TRUE, UINT64_C(5000000000)));
        uint64_t *result = readback;
        for (size_t i = 0; i < 16; ++i) {
            size_t offset = i < 8 ? i : words - 16 + i;
            if (result[i] != word(offset, passes)) {
                fputs("GPU copy mismatch\n", stderr); return 1;
            }
        }
        printf("%s{\"name\":\"%s\",\"memory_type\":%u,\"memory_flags\":%u,"
               "\"bytes\":%" PRIu64 ",\"write_passes\":%u,\"write_gb_s\":%.6f,"
               "\"read_bytes\":%zu,\"read_gb_s\":%.6f,\"gpu_verified\":true}",
               mode ? "," : "", mode ? "device_local_mapped" : "system_cached", type,
               memory.memoryTypes[type].propertyFlags, (uint64_t)bytes, passes,
               (double)bytes * passes / write_seconds / 1e9,
               read_words * sizeof(uint64_t), (double)read_words * sizeof(uint64_t) / read_seconds / 1e9);
        CHECK(vkResetFences(device, 1, &fence));
        CHECK(vkResetCommandBuffer(command, 0));
        vkUnmapMemory(device, target_memory);
        vkUnmapMemory(device, source_memory);
        vkDestroyBuffer(device, target, NULL);
        vkDestroyBuffer(device, source, NULL);
        vkFreeMemory(device, target_memory, NULL);
        vkFreeMemory(device, source_memory, NULL);
    }
    puts("],\"passed\":true}");
    vkDestroyFence(device, fence, NULL);
    vkDestroyCommandPool(device, pool, NULL);
    vkDestroyDevice(device, NULL);
    vkDestroyInstance(instance, NULL);
    return 0;
}
