<script setup lang="ts">
import { auracoderConnectionManager } from "../../../stores/auracoder-connection";
import { auracoderDeviceStore } from "../../../stores/auracoder-device";

const devices = auracoderDeviceStore.devices;
const activeAuraCoderId = auracoderDeviceStore.activeAuraCoderId;
const stateByAuraCoderId = auracoderConnectionManager.stateByAuraCoderId;

function openDetail(auracoderId: string) {
  uni.navigateTo({ url: `/pages/settings/auracoder/detail?auracoderId=${encodeURIComponent(auracoderId)}` });
}

function openAddPage() {
  uni.navigateTo({ url: "/pages/settings/auracoder/add" });
}
</script>

<template>
  <scroll-view class="full-scroll" scroll-y>
    <view class="content-page">
      <view class="section-heading first"><view><text>设备管理</text><text>我的 AuraCoder</text></view><text>{{ devices.length }}</text></view>
      <view v-if="!devices.length" class="empty-state"><text>尚未添加 AuraCoder</text><text>扫码或粘贴配对内容即可添加桌面设备。</text></view>
      <view v-else class="card-list"><button v-for="device in devices" :key="device.auracoderId" class="nav-card" @tap="openDetail(device.auracoderId)"><view class="card-icon">A</view><view class="card-copy"><text>{{ device.name }}<text v-if="device.auracoderId === activeAuraCoderId" class="active-label">当前</text></text><text>{{ device.tunnelId }}</text><text :class="stateByAuraCoderId[device.auracoderId]?.peerOnline ? 'online-text' : ''">{{ stateByAuraCoderId[device.auracoderId]?.peerOnline ? '在线' : '离线' }}</text></view><text class="arrow">›</text></button></view>
      <button class="primary-button add-button" @tap="openAddPage">添加 AuraCoder</button>
    </view>
  </scroll-view>
</template>

<style scoped>
.add-button { margin-top: 20px; }.active-label { margin-left: 6px; padding: 2px 5px; border-radius: 5px; color: var(--accent); background: var(--soft); font-size: 8px; }.online-text { color: var(--accent) !important; }
</style>
