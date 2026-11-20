/* Owned Vulkan capability query: no workload, fence creation or GPU changes. */
#include <stdint.h>
#include <stdio.h>
#include <vulkan/vulkan.h>

int main(void)
{
    VkApplicationInfo app = { .sType=VK_STRUCTURE_TYPE_APPLICATION_INFO, .pApplicationName="legion-sync-caps", .apiVersion=VK_API_VERSION_1_3 };
    VkInstanceCreateInfo create = { .sType=VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, .pApplicationInfo=&app };
    VkInstance instance;
    if (vkCreateInstance(&create,NULL,&instance)!=VK_SUCCESS) return 1;
    VkPhysicalDevice devices[8];
    uint32_t count=8;
    if (vkEnumeratePhysicalDevices(instance,&count,devices)!=VK_SUCCESS || count!=1) {
        vkDestroyInstance(instance,NULL);
        return 1;
    }
    VkPhysicalDeviceProperties properties;
    vkGetPhysicalDeviceProperties(devices[0],&properties);
    VkPhysicalDeviceExternalFenceInfo fence_info = {
        .sType=VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_EXTERNAL_FENCE_INFO,
        .handleType=VK_EXTERNAL_FENCE_HANDLE_TYPE_SYNC_FD_BIT,
    };
    VkExternalFenceProperties fence = { .sType=VK_STRUCTURE_TYPE_EXTERNAL_FENCE_PROPERTIES };
    vkGetPhysicalDeviceExternalFenceProperties(devices[0],&fence_info,&fence);
    VkPhysicalDeviceExternalSemaphoreInfo semaphore_info = {
        .sType=VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_EXTERNAL_SEMAPHORE_INFO,
        .handleType=VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_SYNC_FD_BIT,
    };
    VkExternalSemaphoreProperties semaphore = { .sType=VK_STRUCTURE_TYPE_EXTERNAL_SEMAPHORE_PROPERTIES };
    vkGetPhysicalDeviceExternalSemaphoreProperties(devices[0],&semaphore_info,&semaphore);
    printf("{\"schema\":1,\"vendor_id\":%u,\"device_id\":%u,\"sync_fd_fence_features\":%u,\"sync_fd_semaphore_features\":%u}\n",
           properties.vendorID,properties.deviceID,fence.externalFenceFeatures,semaphore.externalSemaphoreFeatures);
    vkDestroyInstance(instance,NULL);
    return 0;
}
