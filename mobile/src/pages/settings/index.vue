<script setup lang="ts">
function openAuraCoderSettings() {
  uni.navigateTo({ url: "/pages/settings/auracoder/index" });
}
</script>

<template>
  <scroll-view class="full-scroll" scroll-y>
    <view class="content-page">
      <view class="settings-hero"><!-- <view class="pair-logo small">A</view> --><image class="pair-logo small" src="/static/logo.png" mode="aspectFit" /><view><text>设置</text><text>管理手机端的 AuraCoder 连接</text></view></view>
      <view class="settings-group"><button class="settings-row" @tap="openAuraCoderSettings"><view><text>我的 AuraCoder</text><text>添加、切换和管理桌面设备</text></view><text class="arrow">›</text></button></view>
    </view>
  </scroll-view>
</template>

<style scoped>
.settings-row { display: grid; width: 100%; padding: 14px; grid-template-columns: minmax(0, 1fr) auto; align-items: center; color: inherit; text-align: left; }.settings-row view text { display: block; }.settings-row view text:first-child { font-size: 13px; font-weight: 700; }.settings-row view text:last-child { margin-top: 5px; color: var(--muted); font-size: 10px; }
.pair-logo.small { display: block; width: 52px; height: 52px; margin: 0; border: 0; background: transparent; box-shadow: none; }
</style>
