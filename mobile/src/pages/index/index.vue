<script setup lang="ts">
import { computed, ref } from "vue";
import { onLoad, onShow, onUnload } from "@dcloudio/uni-app";
import { auracoderConnectionManager } from "../../stores/auracoder-connection";
import { auracoderDeviceStore } from "../../stores/auracoder-device";
import { workspaceStore } from "../../stores/workspace";

// 历史实现保留：const devices = auracoderDeviceStore.devices;
const allDevices = auracoderDeviceStore.devices;
const enabledDevices = auracoderDeviceStore.enabledDevices;
const activeAuraCoderId = auracoderDeviceStore.activeAuraCoderId;
const stateByAuraCoderId = auracoderConnectionManager.stateByAuraCoderId;
const workspaces = computed(() => activeAuraCoderId.value ? workspaceStore.getItems(activeAuraCoderId.value) : []);
const loading = computed(() => activeAuraCoderId.value ? Boolean(workspaceStore.loadingByAuraCoderId[activeAuraCoderId.value]) : false);
const searchKeyword = ref("");
const moreMenuVisible = ref(false);

const filteredWorkspaces = computed(() => {
  const keyword = searchKeyword.value.trim().toLocaleLowerCase();
  if (!keyword) return workspaces.value;
  return workspaces.value.filter((workspace) => (workspace.name || "未命名项目").toLocaleLowerCase().includes(keyword));
});

let unsubscribeState: (() => void) | undefined;

function goBack() {
  uni.navigateBack();
}

function toggleMoreMenu() {
  moreMenuVisible.value = !moreMenuVisible.value;
}

function runMoreAction(action: "refresh" | "settings") {
  moreMenuVisible.value = false;
  if (action === "refresh") {
    void refreshProjects();
    return;
  }
  openSettings();
}

async function selectAuraCoder(auracoderId: string) {
  auracoderDeviceStore.setActive(auracoderId);
  const state = auracoderConnectionManager.getState(auracoderId);
  if (!state.relayConnected) auracoderConnectionManager.connect(auracoderId);
  if (state.peerOnline) await workspaceStore.load(auracoderId, true);
}

async function refreshProjects() {
  if (!activeAuraCoderId.value) return;
  const state = auracoderConnectionManager.getState(activeAuraCoderId.value);
  if (!state.peerOnline) {
    auracoderConnectionManager.reconnect(activeAuraCoderId.value);
    return;
  }
  await workspaceStore.load(activeAuraCoderId.value, true);
}

function openProject(workspaceId: string) {
  if (!activeAuraCoderId.value) return;
  uni.navigateTo({
    url: `/pages/project/index?auracoderId=${encodeURIComponent(activeAuraCoderId.value)}&workspaceId=${encodeURIComponent(workspaceId)}`,
  });
}

function openSettings() {
  uni.navigateTo({ url: "/pages/settings/index" });
}

function openAuraCoderSettings() {
  uni.navigateTo({ url: "/pages/settings/auracoder/index" });
}

onLoad(() => {
  unsubscribeState = auracoderConnectionManager.subscribeState((auracoderId, state, previous) => {
    if (auracoderId === activeAuraCoderId.value && !previous.peerOnline && state.peerOnline) {
      void workspaceStore.load(auracoderId, true);
    }
  });
});

onShow(() => {
  if (activeAuraCoderId.value && auracoderConnectionManager.getState(activeAuraCoderId.value).peerOnline) {
    void workspaceStore.load(activeAuraCoderId.value);
  }
});

onUnload(() => {
  unsubscribeState?.();
  unsubscribeState = undefined;
});
</script>

<template>
  <view class="mobile-shell home-shell">
    <view class="remote-header">
      <button class="remote-header-button" aria-label="返回" @tap="goBack"><view class="remote-back-icon" /></button>
      <text class="remote-title">Remote</text>
      <view class="remote-header-actions">
        <button class="remote-header-button" aria-label="更多操作" @tap="toggleMoreMenu"><view class="remote-more-icon" /></button>
        <view v-if="moreMenuVisible" class="remote-action-menu">
          <button class="remote-action-button" :disabled="loading" @tap="runMoreAction('refresh')">刷新项目</button>
          <button class="remote-action-button" @tap="runMoreAction('settings')">设置</button>
        </view>
      </view>
    </view>

    <scroll-view class="remote-content-scroll" scroll-y>
      <view class="remote-content-page">
        <scroll-view v-if="enabledDevices.length" class="remote-device-strip" scroll-x>
          <button
            v-for="device in enabledDevices"
            :key="device.auracoderId"
            class="remote-device-chip"
            :class="{ active: device.auracoderId === activeAuraCoderId }"
            @tap="selectAuraCoder(device.auracoderId)"
          >
            <view class="device-online-dot" :class="{ online: stateByAuraCoderId[device.auracoderId]?.peerOnline }" />
            <view class="device-computer-icon" />
            <text class="device-name">{{ device.name }}</text>
          </button>
        </scroll-view>

        <view v-if="!allDevices.length" class="empty-state no-device">
          <text class="empty-logo">A</text>
          <text>尚未添加 AuraCoder</text>
          <text>添加桌面 AuraCoder 后，即可查看项目与会话。</text>
          <button class="primary-button compact-button" @tap="openAuraCoderSettings">前往设置</button>
        </view>

        <view v-else-if="!enabledDevices.length" class="empty-state no-device">
          <text>尚未启用远端 PC</text>
          <text>请在 Remote 设置中开启需要显示的电脑。</text>
          <button class="primary-button compact-button" @tap="openAuraCoderSettings">前往设置</button>
        </view>

        <template v-else>
          <text class="remote-project-title">项目</text>
          <view v-if="loading && !workspaces.length" class="empty-state"><view class="loader" /><text>正在加载项目…</text></view>
          <view v-else-if="!auracoderConnectionManager.getState(activeAuraCoderId || '').peerOnline" class="empty-state">
            <text>当前 AuraCoder 离线</text>
            <text>连接恢复后会保留已加载的项目列表。</text>
            <button class="secondary-button compact-button" @tap="refreshProjects">重新连接</button>
          </view>
          <view v-else-if="!workspaces.length" class="empty-state"><text>0</text><text>桌面 AuraCoder 中还没有项目</text></view>
          <view v-else-if="!filteredWorkspaces.length" class="empty-state remote-filter-empty"><text>未找到匹配项目</text></view>
          <view v-else class="remote-project-list">
            <button v-for="workspace in filteredWorkspaces" :key="workspace.id" class="remote-project-row" @tap="openProject(workspace.id)">
              <view class="project-folder-icon" />
              <text class="project-name">{{ workspace.name || '未命名项目' }}</text>
            </button>
          </view>
        </template>
      </view>
    </scroll-view>

    <view v-if="enabledDevices.length" class="remote-search-bar">
      <view class="search-icon" />
      <input v-model="searchKeyword" class="remote-search-input" placeholder="搜索项目" />
    </view>
  </view>
</template>

<!-- 旧项目页设备区模板保留：原逻辑直接展示全部设备，当前业务改为仅展示启用设备。 -->
<!--
<template>
  <view class="mobile-shell home-shell">
    <scroll-view class="remote-content-scroll" scroll-y>
      <view class="remote-content-page">
        <scroll-view v-if="devices.length" class="remote-device-strip" scroll-x>
          <button v-for="device in devices" :key="device.auracoderId" class="remote-device-chip" :class="{ active: device.auracoderId === activeAuraCoderId }" @tap="selectAuraCoder(device.auracoderId)">
            <view class="device-online-dot" :class="{ online: stateByAuraCoderId[device.auracoderId]?.peerOnline }" />
            <view class="device-computer-icon" />
            <text class="device-name">{{ device.name }}</text>
          </button>
        </scroll-view>
      </view>
    </scroll-view>
  </view>
</template>
-->

<style scoped>
.home-shell { position: relative; height: 100vh; overflow: hidden; }
.remote-header { position: relative; z-index: 3; display: flex; height: calc(62px + env(safe-area-inset-top)); padding: env(safe-area-inset-top) 16px 0; box-sizing: border-box; align-items: center; justify-content: space-between; border-bottom: 1px solid var(--line); background: var(--bg); }
.remote-header-button { display: flex; width: 36px; min-width: 36px; height: 36px; margin: 0; padding: 0; align-items: center; justify-content: center; border: 1px solid var(--line); border-radius: 50%; color: var(--text); background: var(--surface); }
.remote-header-button::after { border: 0; }
.remote-header-actions { position: relative; display: flex; flex: none; }
.remote-title { position: absolute; top: 50%; left: 50%; color: var(--text); font-size: 17px; font-weight: 700; line-height: 1; transform: translate(-50%, -50%); }
.remote-back-icon { width: 10px; height: 10px; border-bottom: 2px solid var(--text); border-left: 2px solid var(--text); transform: rotate(45deg) translate(2px, -2px); }
.remote-more-icon { position: relative; width: 4px; height: 4px; border-radius: 50%; background: var(--text); box-shadow: 0 -7px 0 var(--text), 0 7px 0 var(--text); }
.remote-action-menu { position: absolute; z-index: 4; top: calc(100% + 8px); right: 0; width: 128px; padding: 5px; border: 1px solid var(--line); border-radius: 12px; background: var(--surface); }
.remote-action-button { display: block; width: 100%; min-height: 36px; margin: 0; padding: 0 10px; border: 0; border-radius: 8px; color: var(--text); background: transparent; font-size: 13px; text-align: left; }
.remote-action-button::after { border: 0; }
.remote-action-button:active { background: var(--soft); }
.remote-content-scroll { height: calc(100vh - 62px - env(safe-area-inset-top)); box-sizing: border-box; }
.remote-content-page { min-height: 100%; padding: 18px 16px calc(104px + env(safe-area-inset-bottom)); box-sizing: border-box; }
.remote-device-strip { display: flex; width: 100%; padding: 2px 0 16px; box-sizing: border-box; white-space: nowrap; }
.remote-device-chip { display: inline-flex; width: 132px; min-width: 132px; height: 48px; margin: 0 10px 0 0; padding: 0 12px; flex: none; align-items: center; border: 1px solid var(--line); border-radius: 24px; color: var(--text); background: var(--surface); text-align: left; }
.remote-device-chip:last-child { margin-right: 0; }
.remote-device-chip::after { border: 0; }
.remote-device-chip.active { border-color: var(--accent); background: var(--soft); }
.device-online-dot { width: 7px; height: 7px; margin-right: 8px; flex: none; border-radius: 50%; background: var(--muted); }
.device-online-dot.online { background: var(--accent); }
.device-computer-icon { position: relative; width: 20px; height: 15px; margin-right: 8px; flex: none; border: 1px solid var(--accent); border-radius: 3px; background: var(--soft); }
.device-computer-icon::before { position: absolute; right: 5px; bottom: -5px; left: 5px; height: 3px; border: 1px solid var(--accent); border-top: 0; content: ""; }
.device-computer-icon::after { position: absolute; right: -4px; bottom: -6px; left: -4px; height: 1px; background: var(--accent); content: ""; }
.device-name { min-width: 0; overflow: hidden; color: var(--text); font-size: 12px; font-weight: 600; text-overflow: ellipsis; white-space: nowrap; }
.remote-project-title { display: block; margin: 6px 0 12px; color: var(--text); font-size: 20px; font-weight: 700; }
.remote-project-list { display: flex; flex-direction: column; gap: 8px; }
.remote-project-row { display: flex; width: 100%; min-height: 56px; margin: 0; padding: 0 14px; align-items: center; border: 1px solid var(--line); border-radius: 12px; color: var(--text); background: var(--surface); text-align: left; }
.remote-project-row::after { border: 0; }
.remote-project-row:active { background: var(--soft); }
.project-folder-icon { position: relative; width: 20px; height: 15px; margin-right: 11px; flex: none; border: 1px solid var(--accent); border-radius: 3px; background: var(--soft); }
.project-folder-icon::before { position: absolute; top: -4px; left: 2px; width: 8px; height: 4px; border: 1px solid var(--accent); border-bottom: 0; border-radius: 2px 2px 0 0; background: var(--soft); content: ""; }
.project-name { min-width: 0; overflow: hidden; color: var(--text); font-size: 14px; font-weight: 600; text-overflow: ellipsis; white-space: nowrap; }
.remote-filter-empty { min-height: 160px; }
.remote-search-bar { position: fixed; z-index: 2; right: 16px; bottom: calc(12px + env(safe-area-inset-bottom)); left: 16px; display: flex; min-height: 48px; padding: 0 14px; box-sizing: border-box; align-items: center; border: 1px solid var(--line); border-radius: 24px; background: var(--surface); }
.search-icon { position: relative; width: 15px; height: 15px; margin-right: 9px; flex: none; border: 2px solid var(--muted); border-radius: 50%; box-sizing: border-box; }
.search-icon::after { position: absolute; right: -5px; bottom: -3px; width: 6px; height: 2px; border-radius: 1px; background: var(--muted); content: ""; transform: rotate(45deg); transform-origin: left center; }
.remote-search-input { min-width: 0; height: 44px; flex: 1; padding: 0; color: var(--text); background: transparent; font-size: 14px; }
.remote-search-input::placeholder { color: var(--muted); }
.no-device { min-height: 420px; }
.empty-logo { display: flex; width: 62px; height: 62px; align-items: center; justify-content: center; border: 1px solid var(--accent); border-radius: 18px; color: var(--accent); background: var(--soft); font-size: 25px; font-weight: 800; }
.compact-button { width: 180px; min-height: 42px; margin-top: 12px; }
</style>
