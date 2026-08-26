<script setup lang="ts">
import { onLaunch, onShow, onHide } from "@dcloudio/uni-app";
import { auracoderConnectionManager } from "./stores/auracoder-connection";
import { auracoderDeviceStore } from "./stores/auracoder-device";

onLaunch(() => {
  // console.info("AuraCoder Mobile launched");
  auracoderDeviceStore.initialize();
  auracoderConnectionManager.initialize();
});
onShow(() => {
  // console.log("App Show");
  auracoderConnectionManager.resumeAll();
});
onHide(() => {
  // console.log("App Hide");
  auracoderConnectionManager.keepAliveOnHide();
});
</script>
<style>
@import "./styles.css";

.markdown {
  white-space: pre-wrap;
  word-break: break-word;
}

.mini-button.refresh-button {
  display: flex;
  width: 32px;
  height: 32px;
  padding: 0;
  align-items: center;
  justify-content: center;
  border-radius: 50%;
  line-height: 1;
  text-align: center;
}

.refresh-icon {
  display: block;
  width: 32px;
  height: 32px;
  font-size: 19px;
  line-height: 32px;
  text-align: center;
  transform: translateY(-2px);
}

.thread-heading-actions {
  display: flex;
  align-items: center;
  gap: 8px;
}

.thread-heading-actions .mini-button {
  display: flex;
  width: 32px;
  height: 32px;
  padding: 0;
  align-items: center;
  justify-content: center;
  border-radius: 50%;
  line-height: 1;
}

.thread-heading-actions .create-thread-button {
  color: var(--muted);
  background: rgba(255, 255, 255, 0.045);
}

.thread-heading-actions .thread-refresh-button {
  color: var(--muted);
  background: rgba(255, 255, 255, 0.045);
}

.section-heading .thread-heading-actions .thread-add-icon,
.section-heading .thread-heading-actions .thread-refresh-icon {
  display: flex;
  width: 32px;
  height: 32px;
  margin: 0;
  padding: 0;
  align-items: center;
  justify-content: center;
  color: inherit;
  font-weight: 400;
  letter-spacing: 0;
  line-height: 32px;
}

.section-heading .thread-heading-actions .thread-add-icon {
  font-size: 21px;
  transform: translateY(-1px);
}

.section-heading .thread-heading-actions .thread-refresh-icon {
  font-size: 19px;
  transform: translateY(-2px);
}

.thread-heading-actions .thread-add-glyph,
.thread-heading-actions .thread-refresh-glyph {
  position: relative;
  display: block;
  width: 18px;
  height: 18px;
  flex: none;
  margin: 0;
  padding: 0;
}

.thread-heading-actions .thread-add-glyph > view {
  position: absolute;
  left: 50%;
  top: 50%;
  display: block;
  border-radius: 1px;
  background: currentColor;
  transform: translate(-50%, -50%);
}

.thread-heading-actions .thread-add-glyph > view:first-child {
  width: 14px;
  height: 1px;
}

.thread-heading-actions .thread-add-glyph > view:last-child {
  width: 1px;
  height: 14px;
}

.thread-heading-actions .thread-refresh-ring {
  position: absolute;
  inset: 1px;
  display: block;
  border: 2px solid currentColor;
  border-left-color: transparent;
  border-radius: 50%;
}

.thread-heading-actions .thread-refresh-arrow {
  position: absolute;
  left: 0;
  top: 2px;
  display: block;
  width: 6px;
  height: 6px;
  border-top: 2px solid currentColor;
  border-left: 2px solid currentColor;
  transform: rotate(-16deg);
  transform-origin: center;
}

.thread-heading-actions .thread-refresh-glyph {
  background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='%238d97a7' stroke-width='2.4' stroke-linecap='round' stroke-linejoin='round'%3E%3Cpath d='M4 10a8 8 0 1 1 2.3 5.7'/%3E%3Cpath d='M4 4v6h6'/%3E%3C/svg%3E");
  background-position: center;
  background-repeat: no-repeat;
  background-size: 18px 18px;
}

.section-heading .thread-heading-actions .thread-add-standard-icon,
.section-heading .thread-heading-actions .thread-refresh-standard-icon {
  display: block;
  width: 32px;
  height: 32px;
  margin: 0;
  padding: 0;
  color: inherit;
  font-size: 19px;
  font-weight: 400;
  letter-spacing: 0;
  line-height: 32px;
  text-align: center;
  transform: translateY(-2px);
}

.toolbar-icon {
  display: block;
  width: 18px;
  height: 18px;
  flex: none;
  margin: 0;
  padding: 0;
  background-position: center;
  background-repeat: no-repeat;
  background-size: 18px 18px;
}

.toolbar-icon-add {
  background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='%238d97a7' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'%3E%3Cpath d='M5 12h14'/%3E%3Cpath d='M12 5v14'/%3E%3C/svg%3E");
}

.toolbar-icon-refresh {
  background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='%238d97a7' stroke-width='2' stroke-linecap='round' stroke-linejoin='round'%3E%3Cpath d='M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8'/%3E%3Cpath d='M3 3v5h5'/%3E%3C/svg%3E");
}

.official-toolbar-icon {
  display: flex;
  width: 20px;
  height: 20px;
  flex: none;
  margin: 0;
  padding: 0;
  align-items: center;
  justify-content: center;
  font-weight: 400;
  letter-spacing: 0;
  line-height: 20px;
}

/* uni-icons 通过 ::before 输出字体图标；必须同时居中图形层，不能只居中组件外框。 */
.official-toolbar-icon::before {
  display: flex;
  width: 20px;
  height: 20px;
  align-items: center;
  justify-content: center;
  line-height: 1;
  text-align: center;
}

/* 项目会话页的两个字体图形单独校正；首页刷新不参与位移。 */
.thread-heading-actions .official-toolbar-icon::before {
  transform: translateY(2px);
}

.composer {
  display: flex;
  padding: 10px 12px calc(10px + env(safe-area-inset-bottom));
  flex-direction: column;
  align-items: stretch;
  gap: 9px;
  border-top: 1px solid var(--line);
  background: var(--bg);
}

.composer-meta {
  display: flex;
  min-width: 0;
  padding-left: 0;
  align-items: center;
  gap: 7px;
}

.composer-chip {
  display: block;
  max-width: 48%;
  padding: 7px 11px;
  overflow: hidden;
  border-radius: 999px;
  color: #17181a;
  background: #e7e7e5;
  font-size: 11px;
  font-weight: 700;
  line-height: 1;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.composer-attachments {
  width: 100%;
  white-space: nowrap;
}

.composer-attachment-track {
  display: inline-flex;
  padding-left: 58px;
  gap: 8px;
}

.composer-attachment {
  display: inline-flex;
  width: 210px;
  height: 52px;
  padding: 6px 7px;
  align-items: center;
  gap: 8px;
  border: 1px solid var(--line);
  border-radius: 13px;
  background: var(--surface);
}

.attachment-thumb {
  display: flex;
  width: 38px;
  height: 38px;
  flex: none;
  align-items: center;
  justify-content: center;
  border-radius: 9px;
  color: var(--accent);
  background: var(--soft);
  font-size: 10px;
  font-weight: 800;
}

.attachment-copy {
  min-width: 0;
  flex: 1;
}

.attachment-copy text {
  display: block;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.attachment-copy text:first-child {
  font-size: 10px;
  font-weight: 700;
}

.attachment-copy text:last-child {
  margin-top: 4px;
  color: var(--muted);
  font-size: 8px;
}

.attachment-remove {
  display: flex;
  width: 24px;
  height: 24px;
  flex: none;
  align-items: center;
  justify-content: center;
  border-radius: 50%;
  color: var(--muted);
  background: rgba(255, 255, 255, 0.055);
  font-size: 16px;
}

.composer-row {
  display: grid;
  grid-template-columns: 50px minmax(0, 1fr);
  align-items: end;
  gap: 8px;
}

.attachment-button {
  display: flex;
  width: 50px;
  height: 50px;
  padding: 0 0 3px;
  align-items: center;
  justify-content: center;
  border-radius: 50%;
  color: #0c0d0f;
  background: #f5f5f3;
  font-size: 30px;
  font-weight: 300;
  line-height: 1;
}

.composer-field {
  position: relative;
  min-height: 50px;
  overflow: hidden;
  border-radius: 25px;
  background: #f5f5f3;
}

.composer-field .composer-input {
  width: 100%;
  height: 50px;
  min-height: 50px;
  max-height: 154px;
  padding: 14px 54px 12px 17px;
  border: 0;
  border-radius: 25px;
  color: #17181a;
  background: transparent;
  font-size: 14px;
  line-height: 22px;
}

.composer-action {
  position: absolute;
  right: 5px;
  bottom: 5px;
  display: flex;
  width: 40px;
  height: 40px;
  padding: 0;
  align-items: center;
  justify-content: center;
  border-radius: 50%;
  color: #fff;
  background: #050607;
}

.send-arrow {
  display: block;
  font-size: 28px;
  font-weight: 300;
  line-height: 36px;
  transform: translateY(-2px);
}

.stop-icon {
  display: block;
  font-size: 12px;
}

.waveform-icon {
  display: flex;
  height: 22px;
  align-items: center;
  justify-content: center;
  gap: 3px;
}

.waveform-icon text {
  display: block;
  width: 2px;
  border-radius: 999px;
  background: #fff;
}

.waveform-icon text:nth-child(1),
.waveform-icon text:nth-child(4) {
  height: 10px;
}

.waveform-icon text:nth-child(2) {
  height: 21px;
}

.waveform-icon text:nth-child(3) {
  height: 15px;
}
</style>
