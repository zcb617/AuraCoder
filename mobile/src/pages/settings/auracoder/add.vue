<script setup lang="ts">
import { ref } from "vue";
import { onLoad } from "@dcloudio/uni-app";
import { auracoderConnectionManager } from "../../../stores/auracoder-connection";
import { auracoderDeviceStore } from "../../../stores/auracoder-device";
import type { PairingConfig } from "../../../types";

const pairingText = ref("");
const errorMessage = ref("");
const replacingAuraCoderId = ref("");

function savePairing(raw: string) {
  errorMessage.value = "";
  try {
    const parsed = JSON.parse(raw.trim()) as Partial<PairingConfig>;
    if (parsed.version !== 1 || typeof parsed.endpoint !== "string" || typeof parsed.tunnel_id !== "string"
      || typeof parsed.relay_credential !== "string" || (typeof parsed.pairing_token !== "string" && typeof parsed.device_credential !== "string")) {
      throw new Error("二维码不是有效的 AuraCoder Mobile 配对信息");
    }
    const endpoint = parsed.endpoint.trim();
    const endpointParts = /^(wss?):\/\/(\[[^\]]+\]|[^/:?#]+)(?::\d+)?(?:[/?#]|$)/i.exec(endpoint);
    if (!endpointParts) throw new Error("配对地址不是有效的 WebSocket 地址");
    const protocol = endpointParts[1].toLowerCase();
    const hostname = endpointParts[2].toLowerCase();
    if (protocol !== "wss" && !(protocol === "ws" && ["127.0.0.1", "localhost", "[::1]"].includes(hostname))) {
      throw new Error("正式配对地址必须使用 WSS 加密连接");
    }
    if (!parsed.tunnel_id || parsed.relay_credential.length < 32 || (parsed.pairing_token || parsed.device_credential || "").length < 32) {
      throw new Error("配对凭据不完整，请刷新二维码");
    }
    if (parsed.expires_at && new Date(parsed.expires_at).getTime() <= Date.now()) throw new Error("配对二维码已经过期，请刷新后重试");
    const device = auracoderDeviceStore.addOrReplace({
      version: 1,
      endpoint,
      tunnel_id: parsed.tunnel_id,
      relay_credential: parsed.relay_credential,
      pairing_token: parsed.pairing_token,
      device_credential: parsed.device_credential,
      expires_at: parsed.expires_at,
    }, replacingAuraCoderId.value || undefined);
    auracoderConnectionManager.connect(device.auracoderId);
    uni.showToast({ title: replacingAuraCoderId.value ? '已更新配对信息' : '已添加 AuraCoder', icon: 'success' });
    setTimeout(() => uni.navigateBack(), 500);
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : String(error);
  }
}

function scanPairing() {
  errorMessage.value = "";
  uni.scanCode({
    onlyFromCamera: true,
    scanType: ["qrCode"],
    success: (result) => savePairing(result.result),
    fail: (error) => {
      if (!String(error.errMsg || "").toLowerCase().includes("cancel")) errorMessage.value = `无法扫描二维码：${error.errMsg || '请检查相机权限'}`;
    },
  });
}

onLoad((query) => { replacingAuraCoderId.value = String((query || {}).auracoderId || ""); });
</script>

<template>
  <scroll-view class="full-scroll" scroll-y>
    <view class="pair-page compact-pair-page">
      <view class="pair-brand"><view class="pair-logo">A</view><text class="pair-name">AuraCoder Mobile</text><text class="pair-title">{{ replacingAuraCoderId ? '重新配对 AuraCoder' : '添加 AuraCoder' }}</text><text class="pair-copy">从桌面 AuraCoder 的远程访问设置中扫描二维码，或粘贴配对内容。</text></view>
      <button class="primary-button" @tap="scanPairing">扫码配对</button>
      <view class="divider"><view/><text>或粘贴配对内容</text><view/></view>
      <textarea v-model="pairingText" class="pair-input" placeholder="粘贴 AuraCoder Mobile 配对内容" placeholder-class="placeholder" :maxlength="20000"/>
      <button class="secondary-button" :disabled="!pairingText.trim()" @tap="savePairing(pairingText)">验证并保存</button>
      <text v-if="errorMessage" class="form-error">{{ errorMessage }}</text><text class="security">配对凭据仅保存在当前设备的 uni-app Storage 中。</text>
    </view>
  </scroll-view>
</template>

<style scoped>
.compact-pair-page { min-height: 100vh; padding-top: calc(20px + env(safe-area-inset-top)); padding-bottom: calc(30px + env(safe-area-inset-bottom)); justify-content: flex-start; }.placeholder { color: #6c7686; }
</style>
