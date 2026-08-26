<script setup lang="ts">
import { computed, ref } from "vue";
import { onLoad, onShow } from "@dcloudio/uni-app";
import { conversationStore } from "../../../stores/conversation";
import { auracoderConnectionManager } from "../../../stores/auracoder-connection";
import { auracoderDeviceStore } from "../../../stores/auracoder-device";
import { projectStore } from "../../../stores/project";
import { workspaceStore } from "../../../stores/workspace";

const auracoderId = ref("");
const name = ref("");
const device = computed(() => auracoderDeviceStore.getDevice(auracoderId.value));
const state = computed(() => auracoderConnectionManager.getState(auracoderId.value));

function saveName() {
  if (!device.value) return;
  auracoderDeviceStore.rename(auracoderId.value, name.value);
  name.value = auracoderDeviceStore.getDevice(auracoderId.value)?.name || name.value;
  uni.showToast({ title: '名称已保存', icon: 'success' });
}

function removeDevice() {
  if (!device.value) return;
  uni.showModal({ title: '解除绑定', content: `将只删除“${device.value.name}”及其本地缓存，其他 AuraCoder 不会受影响。`, confirmText: '解除绑定', confirmColor: '#f2776e', success: (result) => {
    if (!result.confirm) return;
    auracoderConnectionManager.remove(auracoderId.value);
    workspaceStore.clear(auracoderId.value);
    projectStore.clear(auracoderId.value);
    conversationStore.clear(auracoderId.value);
    auracoderDeviceStore.remove(auracoderId.value);
    uni.navigateBack();
  } });
}

function openRepairPage() {
  uni.navigateTo({ url: `/pages/settings/auracoder/add?auracoderId=${encodeURIComponent(auracoderId.value)}` });
}

onLoad((query) => {
  auracoderId.value = String((query || {}).auracoderId || "");
  if (!auracoderDeviceStore.getDevice(auracoderId.value)) {
    uni.showToast({ title: 'AuraCoder 不存在', icon: 'none' });
    uni.navigateBack();
    return;
  }
  name.value = auracoderDeviceStore.getDevice(auracoderId.value)?.name || "";
});

onShow(() => { if (auracoderId.value) name.value = auracoderDeviceStore.getDevice(auracoderId.value)?.name || name.value; });
</script>

<template>
  <scroll-view class="full-scroll" scroll-y>
    <view v-if="device" class="content-page">
      <view class="section-heading first"><view><text>设备信息</text><text>{{ device.name }}</text></view><text :class="state.peerOnline ? 'online-text' : ''">{{ state.peerOnline ? '在线' : '离线' }}</text></view>
      <view class="form-card"><text>显示名称</text><input v-model="name" class="name-input" maxlength="40"/><button class="mini-button" @tap="saveName">保存</button></view>
      <view class="settings-group"><view><text>设备标识</text><text>{{ device.tunnelId }}</text></view><view><text>Relay 地址</text><text>{{ device.endpoint }}</text></view><view><text>最后连接</text><text>{{ device.lastConnectedAt || '尚未连接' }}</text></view><view><text>当前状态</text><text>{{ state.peerOnline ? '桌面在线' : state.relayConnected ? '等待桌面上线' : '正在连接 Relay' }}</text></view></view>
      <button class="secondary-button action-button" @tap="auracoderDeviceStore.setActive(auracoderId)">设为当前 AuraCoder</button>
      <button class="secondary-button action-button" @tap="auracoderConnectionManager.reconnect(auracoderId)">重新连接</button>
      <button class="secondary-button action-button" @tap="openRepairPage">重新配对</button>
      <button class="danger-button" @tap="removeDevice">解除绑定</button>
    </view>
  </scroll-view>
</template>

<style scoped>
.form-card { display: grid; margin-bottom: 14px; padding: 13px; grid-template-columns: 70px minmax(0, 1fr) 48px; align-items: center; gap: 8px; border: 1px solid var(--line); border-radius: 14px; background: var(--surface); font-size: 11px; }.name-input { width: 100%; height: 34px; padding: 0 8px; border-radius: 8px; color: var(--text); background: rgba(255,255,255,.05); font-size: 12px; }.action-button { margin-top: 12px; }.online-text { color: var(--accent); }
</style>
