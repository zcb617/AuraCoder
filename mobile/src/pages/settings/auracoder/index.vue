<script setup lang="ts">
import { ref } from "vue";
import type { PairedAuraCoder } from "../../../types";
import { auracoderConnectionManager } from "../../../stores/auracoder-connection";
import { auracoderDeviceStore } from "../../../stores/auracoder-device";

const devices = auracoderDeviceStore.devices;
// 历史实现保留：const activeAuraCoderId = auracoderDeviceStore.activeAuraCoderId;
const stateByAuraCoderId = auracoderConnectionManager.stateByAuraCoderId;
const DEVICE_ROW_HEIGHT = 80;
const draggingAuraCoderId = ref<string | null>(null);
const dragStartIndex = ref(-1);
const dragCurrentIndex = ref(-1);
const dragStartY = ref(0);

/** 进入设备详情页，查看并维护该远程 PC 的连接配置。 */
function openDetail(auracoderId: string) {
  uni.navigateTo({ url: `/pages/settings/auracoder/detail?auracoderId=${encodeURIComponent(auracoderId)}` });
}

/** 进入添加连接页，通过二维码或配对内容新增远程 PC。 */
function openAddPage() {
  uni.navigateTo({ url: "/pages/settings/auracoder/add" });
}

/** 切换设备启用状态，并同步维护该设备的远程连接。 */
function toggleEnabled(device: PairedAuraCoder, event: Event) {
  event.stopPropagation();
  const enabled = Boolean((event as Event & { detail?: { value?: boolean } }).detail?.value);
  auracoderDeviceStore.setEnabled(device.auracoderId, enabled);
  if (enabled) auracoderConnectionManager.connect(device.auracoderId);
  else auracoderConnectionManager.disconnect(device.auracoderId);
}

type TouchPoint = {
  /** 当前触摸点在页面中的纵向坐标。 */
  clientY?: number;
};
type TouchEventLike = Event & { touches?: ArrayLike<TouchPoint>; changedTouches?: ArrayLike<TouchPoint> };

/** 从拖动触摸事件取得当前指针纵坐标。 */
function readTouchY(event: TouchEventLike) {
  const point = event.touches?.[0] || event.changedTouches?.[0];
  return typeof point?.clientY === "number" ? point.clientY : null;
}

/** 长按设备行左侧手柄后开始排序拖动。 */
function beginDrag(auracoderId: string, event: TouchEventLike) {
  event.stopPropagation();
  event.preventDefault();
  const clientY = readTouchY(event);
  const index = devices.value.findIndex((item) => item.auracoderId === auracoderId);
  if (clientY === null || index < 0) return;
  draggingAuraCoderId.value = auracoderId;
  dragStartIndex.value = index;
  dragCurrentIndex.value = index;
  dragStartY.value = clientY;
}

/** 根据纵向位移逐行调整设备顺序，并阻止页面滚动。 */
function moveDrag(event: TouchEventLike) {
  if (!draggingAuraCoderId.value) return;
  event.stopPropagation();
  event.preventDefault();
  const clientY = readTouchY(event);
  if (clientY === null) return;
  const offset = Math.round((clientY - dragStartY.value) / DEVICE_ROW_HEIGHT);
  const target = Math.max(0, Math.min(devices.value.length - 1, dragStartIndex.value + offset));
  if (target === dragCurrentIndex.value) return;
  auracoderDeviceStore.reorder(dragCurrentIndex.value, target);
  dragStartIndex.value = target;
  dragCurrentIndex.value = target;
  dragStartY.value = clientY;
}

/** 结束排序拖动并清理拖动状态。 */
function endDrag(event: TouchEventLike) {
  if (draggingAuraCoderId.value) {
    event.stopPropagation();
    event.preventDefault();
  }
  draggingAuraCoderId.value = null;
  dragStartIndex.value = -1;
  dragCurrentIndex.value = -1;
  dragStartY.value = 0;
}

/** 取消排序拖动并清理拖动状态。 */
function cancelDrag(event: TouchEventLike) {
  endDrag(event);
}
</script>

<template>
  <scroll-view class="full-scroll" scroll-y>
    <view class="content-page remote-settings-page">
      <view class="section-heading first"><view><text>连接</text><text>远端 PC</text></view><text>{{ devices.length }}</text></view>
      <view class="device-group">
        <view v-for="device in devices" :key="device.auracoderId" class="device-row" :class="{ dragging: device.auracoderId === draggingAuraCoderId }">
          <view class="drag-handle" aria-label="拖动排序" @longpress="beginDrag(device.auracoderId, $event)" @touchmove="moveDrag($event)" @touchend="endDrag($event)" @touchcancel="cancelDrag($event)"><view class="handle-lines" /></view>
          <view class="device-row-content" @tap="openDetail(device.auracoderId)">
            <view class="device-computer-icon" />
            <view class="device-copy"><text class="device-name">{{ device.name }}</text><text class="device-state" :class="{ online: stateByAuraCoderId[device.auracoderId]?.peerOnline }">{{ stateByAuraCoderId[device.auracoderId]?.peerOnline ? '在线' : '离线' }}</text></view>
          </view>
          <switch class="device-switch" :checked="device.enabled" color="#76e6b5" @change="toggleEnabled(device, $event)" />
        </view>
        <button class="add-connection-row" @tap="openAddPage"><view class="add-icon">+</view><view class="add-copy"><text>添加连接</text><text>扫描桌面 AuraCoder 二维码进行配对</text></view><text class="arrow">›</text></button>
      </view>
      <text class="drag-hint">长按拖动手柄，然后拖动以重新排序。</text>
    </view>
  </scroll-view>
</template>

<!-- 旧设备管理模板保留：原页面使用设备按钮、UUID 与当前标记；其业务已由上方 Remote 连接区替代。 -->
<!--
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
-->

<style scoped>
.remote-settings-page { padding-bottom: calc(36px + env(safe-area-inset-bottom)); }
.device-group { overflow: hidden; border: 1px solid var(--line); border-radius: 16px; background: var(--surface); }
.device-row { display: flex; height: 80px; box-sizing: border-box; align-items: center; border-bottom: 1px solid var(--line); }
.device-row.dragging { background: var(--soft); }
.drag-handle { display: flex; width: 48px; height: 80px; flex: none; align-items: center; justify-content: center; touch-action: none; }
.handle-lines { position: relative; width: 17px; height: 14px; border-top: 2px solid var(--muted); border-bottom: 2px solid var(--muted); }
.handle-lines::after { position: absolute; top: 5px; right: 0; left: 0; height: 2px; background: var(--muted); content: ""; }
.device-row-content { display: flex; min-width: 0; height: 80px; flex: 1; align-items: center; }
.device-computer-icon { position: relative; width: 27px; height: 20px; margin-right: 13px; flex: none; border: 1px solid var(--accent); border-radius: 4px; background: var(--soft); }
.device-computer-icon::before { position: absolute; right: 8px; bottom: -6px; left: 8px; height: 4px; border: 1px solid var(--accent); border-top: 0; content: ""; }
.device-computer-icon::after { position: absolute; right: -5px; bottom: -7px; left: -5px; height: 1px; background: var(--accent); content: ""; }
.device-copy { display: flex; min-width: 0; flex-direction: column; gap: 4px; }
.device-name { overflow: hidden; color: var(--text); font-size: 15px; font-weight: 650; text-overflow: ellipsis; white-space: nowrap; }
.device-state { color: var(--muted); font-size: 12px; }
.device-state.online { color: var(--accent); }
.device-switch { margin: 0 14px 0 10px; transform: scale(.82); transform-origin: center; }
.add-connection-row { display: flex; width: 100%; height: 80px; margin: 0; padding: 0 15px; box-sizing: border-box; align-items: center; border: 0; border-radius: 0; color: var(--text); background: transparent; text-align: left; }
.add-connection-row::after { border: 0; }
.add-connection-row:active { background: var(--soft); }
.add-icon { display: flex; width: 27px; height: 27px; margin: 0 13px 0 0; align-items: center; justify-content: center; border: 1px solid var(--accent); border-radius: 50%; color: var(--accent); font-size: 24px; font-weight: 300; line-height: 1; }
.add-copy { display: flex; min-width: 0; flex: 1; flex-direction: column; gap: 4px; }
.add-copy text:first-child { color: var(--text); font-size: 15px; font-weight: 650; }
.add-copy text:last-child { overflow: hidden; color: var(--muted); font-size: 11px; text-overflow: ellipsis; white-space: nowrap; }
.arrow { margin-left: 10px; color: var(--muted); font-size: 28px; font-weight: 300; }
.drag-hint { display: block; margin: 14px 4px 0; color: var(--muted); font-size: 12px; }

/* 旧页面样式保留，供历史版本追溯；当前连接区使用上方独立样式。 */
/* .add-button { margin-top: 20px; }.active-label { margin-left: 6px; padding: 2px 5px; border-radius: 5px; color: var(--accent); background: var(--soft); font-size: 8px; }.online-text { color: var(--accent) !important; } */
</style>
