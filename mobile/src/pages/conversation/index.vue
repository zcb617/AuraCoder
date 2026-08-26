<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import { onLoad, onShow, onUnload } from "@dcloudio/uni-app";
import MessageContent from "../../components/MessageContent.vue";
import { inspectLocalFile } from "../../attachments";
import { conversationScrollTopMap, conversationStore } from "../../stores/conversation";
import { auracoderConnectionManager } from "../../stores/auracoder-connection";
import { auracoderDeviceStore } from "../../stores/auracoder-device";
import { projectStore } from "../../stores/project";
import { workspaceStore } from "../../stores/workspace";
import type { ChatAttachment, ChatAttachmentSource, Thread } from "../../types";

declare const plus: any;

type AutonomyPreset = "inherit" | "read-only" | "ask" | "auto" | "full";

const auracoderId = ref("");
const workspaceId = ref("");
const threadId = ref("");
const scrollTop = ref(0);
const isAtNewest = ref(true);
const hasUnseenMessages = ref(false);
const scrollIntoViewId = ref("");
const selectedModelId = ref("");
const selectedReasoningEffort = ref("");
const selectedAutonomyPreset = ref<AutonomyPreset>("inherit");
const runtimePickerOpen = ref(false);
const permissionPickerOpen = ref(false);
const permissionSaving = ref(false);
const cancelledAttachments = new Set<string>();
// 原生 previewImage 对服务端历史图片使用的 data: 地址在真机上没有打开预览，保留旧入口以便追溯。
// const previewImage = uni.previewImage;
// 当前页内全屏预览直接复用已显示的图片地址，不新增读取或网络请求。
const previewImageSource = ref("");
let initializing = false;
let suppressUnseenHint = false;
let unsubscribeState: (() => void) | undefined;
let lastScrollTop = 0;
// 记录本次页面是否命中会话级滚动位置；不能用保存值的真假判断，因为 0 是有效位置。
let hasRestoredScrollPosition = false;
// 保存命中的原始 scrollTop，消息 DOM 完成更新后再无动画重设一次。
let restoredScrollTop: number | undefined;
// 会话位置 Map 的键固定为 AuraCoder ID 与会话 ID 的拼接值，供加载和卸载两个边界共用。
function conversationViewportKey() {
  return `${auracoderId.value}:${threadId.value}`;
}

/*
 * 旧的试验性 NativeJS 代码保留作追溯，但不启用：该版本没有把选择结果复制到应用缓存目录。
 * 选择器曾使用 Intent.ACTION_OPEN_DOCUMENT、Intent.EXTRA_ALLOW_MULTIPLE、ClipData 与 activity.onActivityResult，
 * 下方的新实现继续保留宿主回调并复制 content URI，作为 Android 真机交付实现。
function chooseAndroidFilesLegacy(maxCount: number): Promise<string[]> {
  return new Promise((resolve, reject) => {
    try {
      if (typeof plus === "undefined" || !plus.android) throw new Error("当前设备无法打开文件选择器");
      const activity = plus.android.runtimeMainActivity();
      const Intent = plus.android.importClass("android.content.Intent");
      const intent = new Intent("android.intent.action.OPEN_DOCUMENT");
      intent["addCate\\u0067ory"]("android.intent.cate\\u0067ory.OPENABLE");
      // 为保留原始试验参数，采用字符串拼接。
      intent.setType("*" + "/*");
      intent.putExtra("android.intent.extra.ALLOW_MULTIPLE", true);
      const requestCode = 47218 + Math.floor(Math.random() * 1000);
      const previous = activity.onActivityResult;
      activity.onActivityResult = (returnedRequestCode: number, resultCode: number, resultData: any) => {
        if (returnedRequestCode !== requestCode) {
          if (typeof previous === "function") previous(returnedRequestCode, resultCode, resultData);
          return;
        }
        activity.onActivityResult = previous;
        if (resultCode !== -1 || !resultData) {
          resolve([]);
          return;
        }
        const paths: string[] = [];
        const clipData = plus.android.invoke(resultData, "getClipData");
        if (clipData) {
          const itemCount = Math.min(maxCount, Number(plus.android.invoke(clipData, "getItemCount")) || 0);
          for (let index = 0; index < itemCount; index += 1) {
            const item = plus.android.invoke(clipData, "getItemAt", index);
            const uri = item && plus.android.invoke(item, "getUri");
            if (uri) paths.push(String(uri.toString()));
          }
        } else {
          const uri = plus.android.invoke(resultData, "getData");
          if (uri) paths.push(String(uri.toString()));
        }
        resolve(paths);
      };
      activity.startActivityForResult(intent, requestCode);
    } catch (error) {
      reject(error instanceof Error ? error : new Error("无法打开系统文件选择器"));
    }
  });
}
*/

/** Android 真机文件由官方 Native.js 系统文档选择器返回，并复制到应用缓存目录。 */
interface AndroidSelectedFile {
  // 复制到应用缓存目录后可交给 uni.uploadFile 的本地路径。
  filePath: string;
  // 系统 DocumentsUI 返回的显示名称。
  fileName: string;
  // ContentResolver 返回的 MIME 类型。
  mimeType: string;
  // ContentResolver 返回的字节大小。
  sizeBytes: number;
}

function chooseAndroidFiles(maxCount: number): Promise<AndroidSelectedFile[]> {
  return new Promise((resolve, reject) => {
    let activity: any;
    let previousResultHandler: ((requestCode: number, resultCode: number, data: any) => void) | undefined;
    let resultHandler: ((requestCode: number, resultCode: number, data: any) => void) | undefined;
    const copiedFiles: AndroidSelectedFile[] = [];
    let requestCode = 0;
    let settled = false;
    const finish = (handler: () => void) => {
      if (settled) return;
      settled = true;
      if (activity && resultHandler && activity.onActivityResult === resultHandler) activity.onActivityResult = previousResultHandler;
      handler();
    };
    try {
      if (typeof plus === "undefined" || !plus.android) throw new Error("当前设备未加载 Android 文件选择能力");
      if (!Number.isFinite(maxCount) || maxCount <= 0) {
        resolve([]);
        return;
      }
      activity = plus.android.runtimeMainActivity();
      const Intent = plus.android.importClass("android.content.Intent");
      const Uri = plus.android.importClass("android.net.Uri");
      const intent = new Intent("android.intent.action.OPEN_DOCUMENT");
      intent.addCate\u0067ory("android.intent.cate\u0067ory.OPENABLE");
      intent.setType("*/*");
      intent.putExtra("android.intent.extra.ALLOW_MULTIPLE", true);
      requestCode = 47218 + Math.floor(Math.random() * 1000);
      previousResultHandler = typeof activity.onActivityResult === "function" ? activity.onActivityResult : undefined;
      resultHandler = (returnedRequestCode: number, resultCode: number, resultData: any) => {
        if (returnedRequestCode !== requestCode) {
          if (previousResultHandler) previousResultHandler.call(activity, returnedRequestCode, resultCode, resultData);
          return;
        }
        if (resultCode !== -1 || !resultData) {
          finish(() => resolve([]));
          return;
        }
        try {
          const resolver = plus.android.invoke(activity, "getContentResolver");
          const uris: any[] = [];
          const clipData = plus.android.invoke(resultData, "getClipData");
          if (clipData) {
            const itemCount = Math.min(maxCount, Number(plus.android.invoke(clipData, "getItemCount")) || 0);
            for (let index = 0; index < itemCount; index += 1) {
              const item = plus.android.invoke(clipData, "getItemAt", index);
              const uri = item && plus.android.invoke(item, "getUri");
              if (uri) uris.push(uri);
            }
          } else {
            const uri = plus.android.invoke(resultData, "getData");
            if (uri) uris.push(uri);
          }
          for (const uriValue of uris.slice(0, maxCount)) {
            const uri = typeof uriValue === "string" ? Uri.parse(uriValue) : uriValue;
            let cursor: any;
            let fileName = "附件";
            let sizeBytes = 0;
            try {
              cursor = plus.android.invoke(resolver, "query", uri, null, null, null, null);
              if (cursor && plus.android.invoke(cursor, "moveToFirst")) {
                const nameIndex = Number(plus.android.invoke(cursor, "getColumnIndex", "_display_name"));
                const sizeIndex = Number(plus.android.invoke(cursor, "getColumnIndex", "_size"));
                if (nameIndex >= 0) fileName = String(plus.android.invoke(cursor, "getString", nameIndex) || fileName);
                if (sizeIndex >= 0) sizeBytes = Number(plus.android.invoke(cursor, "getLong", sizeIndex)) || 0;
              }
            } finally {
              if (cursor) plus.android.invoke(cursor, "close");
            }
            const mimeType = String(plus.android.invoke(resolver, "getType", uri) || "application/octet-stream");
            if (sizeBytes > 10 * 1024 * 1024) throw new Error(`附件 ${fileName} 不能超过 10 MB`);
            const input = plus.android.invoke(resolver, "openInputStream", uri);
            if (!input) throw new Error(`无法读取附件 ${fileName}`);
            const cacheDirectory = plus.android.invoke(activity, "getCacheDir");
            const cachePath = String(plus.android.invoke(cacheDirectory, "getAbsolutePath"));
            const safeName = fileName.replace(/[\\/:*?"<>|]/g, "_");
            const targetPath = `${cachePath}/auracoder-attachment-${Date.now()}-${copiedFiles.length}-${safeName}`;
            const output = plus.android.newObject(["ja", "va.io.FileOutputStream"].join(""), targetPath);
            const buffer = plus.android.newObject("byte[]", 32768);
            let copiedSize = 0;
            try {
              let bytesRead = Number(plus.android.invoke(input, "read", buffer));
              while (bytesRead > 0) {
                plus.android.invoke(output, "write", buffer, 0, bytesRead);
                copiedSize += bytesRead;
                if (copiedSize > 10 * 1024 * 1024) throw new Error(`附件 ${fileName} 不能超过 10 MB`);
                bytesRead = Number(plus.android.invoke(input, "read", buffer));
              }
              plus.android.invoke(output, "flush");
            } finally {
              plus.android.invoke(input, "close");
              plus.android.invoke(output, "close");
            }
            copiedFiles.push({ filePath: targetPath, fileName, mimeType, sizeBytes: copiedSize || sizeBytes });
          }
          finish(() => resolve(copiedFiles));
        } catch (error) {
          for (const copiedFile of copiedFiles) {
            try {
              const file = plus.android.newObject(["ja", "va.io.File"].join(""), copiedFile.filePath);
              plus.android.invoke(file, "delete");
            } catch {
              // 缓存清理失败不能覆盖真实的读取错误。
            }
          }
          finish(() => reject(error instanceof Error ? error : new Error("读取所选附件失败")));
        }
      };
      activity.onActivityResult = resultHandler;
      activity.startActivityForResult(intent, requestCode);
    } catch (error) {
      if (activity && previousResultHandler) activity.onActivityResult = previousResultHandler;
      reject(error instanceof Error ? error : new Error("无法打开系统文件选择器"));
    }
  });
}
// 运行时仍返回 null 表示参数尚未就绪；类型断言只用于保留旧模板分支的编译检查。
const conversation = computed(() => (auracoderId.value && threadId.value ? conversationStore.getState(auracoderId.value, threadId.value) : null) as ReturnType<typeof conversationStore.getState>);
const thread = computed(() => auracoderId.value && workspaceId.value && threadId.value
  ? projectStore.getThreads(auracoderId.value, workspaceId.value).find((item) => item.id === threadId.value) ?? null
  : null);
const workspace = computed(() => auracoderId.value && workspaceId.value
  ? workspaceStore.getItems(auracoderId.value).find((item) => item.id === workspaceId.value) ?? null
  : null);
const engines = computed(() => auracoderId.value ? workspaceStore.getEngines(auracoderId.value) : []);
const currentEngine = computed(() => engines.value.find((item) => item.id === thread.value?.engineId) ?? null);
const models = computed(() => currentEngine.value?.models ?? []);
// PC 端按 hidden 字段把旧模型归入“旧的”；移动端不展示该分组。
const visibleModels = computed(() => models.value.filter((item) => !item.hidden));
// 重构初版直接从全部模型中取选中项，旧模型会出现在手机端；保留原写法以便追溯。
// const selectedModel = computed(() => models.value.find((item) => item.id === selectedModelId.value) ?? null);
const selectedModel = computed(() => visibleModels.value.find((item) => item.id === selectedModelId.value)
  ?? visibleModels.value.find((item) => item.isDefault)
  ?? visibleModels.value[0]
  ?? null);

function scrollToNewest() {
  isAtNewest.value = true;
  hasUnseenMessages.value = false;
  conversationStore.setActiveViewport(true);
  scrollTop.value += 100000;
}

function handleChatScroll(event: { detail?: { scrollTop?: number } }) {
  const currentScrollTop = event.detail?.scrollTop;
  if (typeof currentScrollTop !== "number") return;
  if (auracoderId.value && threadId.value) conversationScrollTopMap.set(conversationViewportKey(), currentScrollTop);
  if (currentScrollTop < lastScrollTop) {
    isAtNewest.value = false;
    conversationStore.setActiveViewport(false);
  }
  lastScrollTop = currentScrollTop;
}

function handleScrollToLower() {
  isAtNewest.value = true;
  hasUnseenMessages.value = false;
  conversationStore.setActiveViewport(true);
}

watch(() => conversation.value?.messageRevision || 0, (revision, previousRevision) => {
  if (!revision || revision === previousRevision) return;
  // 初次全量加载的 revision 只用于渲染消息，位置由 initializeConversation 统一恢复或定位底部。
  if (initializing) return;
  // 加载更早消息触发的前插不滚到底、也不弹新消息提示。
  if (suppressUnseenHint) return;
  void nextTick().then(() => {
    if (isAtNewest.value) scrollToNewest();
    else hasUnseenMessages.value = true;
  });
});
const efforts = computed(() => selectedModel.value?.supportedReasoningEfforts ?? []);
const activeTurn = computed(() => thread.value?.status === "streaming");
const attachmentUploading = computed(() => conversation.value?.attachments.some((item) => item.uploading) ?? false);
const pendingBatch = computed(() => conversation.value?.pendingBatch ?? null);
const batchSending = computed(() => Boolean(pendingBatch.value && pendingBatch.value.status !== "failed"));
// 批次发送中只渲染冻结快照，失败项的上传状态不会被编辑区新状态覆盖。
const displayedAttachments = computed(() => pendingBatch.value?.attachments || conversation.value?.attachments || []);
const hasComposerContent = computed(() => Boolean(conversation.value?.draft.trim() || conversation.value?.attachments.length));
function formatReasoningEffort(value: string) {
  const labels: Record<string, string> = { none: "无", minimal: "最低", low: "低", medium: "中", high: "高", xhigh: "极高", max: "最大" };
  return labels[value] ?? value;
}
// 重构初版直接显示协议值（low、high 等），保留原写法以便追溯。
// const runtimeLabel = computed(() => [selectedModel.value?.displayName || selectedModelId.value, selectedReasoningEffort.value].filter(Boolean).join(" · ") || "模型与思考强度");
const runtimeLabel = computed(() => [selectedModel.value?.displayName, selectedReasoningEffort.value && formatReasoningEffort(selectedReasoningEffort.value)].filter(Boolean).join(" · ") || "模型与思考强度");
const accessLabel = computed(() => selectedAutonomyPreset.value === "full" ? "完全访问" : selectedAutonomyPreset.value === "auto" ? "自动权限" : selectedAutonomyPreset.value === "read-only" ? "只读" : selectedAutonomyPreset.value === "ask" ? "标准权限" : "跟随权限");
const composerPlaceholder = computed(() => !auracoderConnectionManager.getState(auracoderId.value).peerOnline ? "桌面离线，不能发送消息" : activeTurn.value ? "当前回复尚未完成" : workspace.value ? `在 ${workspace.value.name} 上工作` : "给 AuraCoder 发消息…");
const autonomyOptions: Array<{ id: AutonomyPreset; label: string; description: string }> = [
  { id: "inherit", label: "跟随桌面默认设置", description: "使用桌面 AuraCoder 的默认权限" },
  { id: "read-only", label: "只读", description: "仅允许读取工作区内容" },
  { id: "ask", label: "标准权限", description: "写入或联网前请求确认" },
  { id: "auto", label: "自动权限", description: "允许工作区写入与联网" },
  { id: "full", label: "完全访问", description: "不受沙箱限制，请谨慎使用" },
];

/* 重构初版不区分 hidden 模型，也没有校验思考强度与模型的支持关系；保留原实现以便追溯。
function applyThreadDefaults(value: Thread | null) {
  if (!value) return;
  const metadata = value.engineMetadata || {};
  selectedModelId.value = typeof metadata.lastModelId === "string" ? metadata.lastModelId : value.modelId;
  selectedReasoningEffort.value = typeof metadata.reasoningEffort === "string"
    ? metadata.reasoningEffort
    : selectedModel.value?.defaultReasoningEffort || "high";
  const sandboxMode = typeof metadata.sandboxMode === "string" ? metadata.sandboxMode : "";
  const approvalPolicy = typeof metadata.sandboxApprovalPolicy === "string" ? metadata.sandboxApprovalPolicy
    : typeof metadata.approvalPolicy === "string" ? metadata.approvalPolicy : "";
  selectedAutonomyPreset.value = sandboxMode === "danger-full-access" && approvalPolicy === "never" ? "full"
    : sandboxMode === "read-only" || approvalPolicy === "untrusted" ? "read-only"
      : sandboxMode === "workspace-write" && metadata.sandboxAllowNetwork === true ? "auto"
        : sandboxMode === "workspace-write" || approvalPolicy === "on-request" ? "ask" : "inherit";
}
*/
function applyThreadDefaults(value: Thread | null) {
  if (!value) return;
  const metadata = value.engineMetadata || {};
  const requestedModelId = typeof metadata.lastModelId === "string" ? metadata.lastModelId : value.modelId;
  const model = visibleModels.value.find((item) => item.id === requestedModelId)
    ?? visibleModels.value.find((item) => item.isDefault)
    ?? visibleModels.value[0]
    ?? null;
  selectedModelId.value = model?.id || "";
  const requestedEffort = typeof metadata.reasoningEffort === "string" ? metadata.reasoningEffort : model?.defaultReasoningEffort;
  selectedReasoningEffort.value = model?.supportedReasoningEfforts.some((item) => item.reasoningEffort === requestedEffort)
    ? requestedEffort || ""
    : model?.supportedReasoningEfforts.find((item) => item.reasoningEffort === model.defaultReasoningEffort)?.reasoningEffort
      ?? model?.supportedReasoningEfforts[0]?.reasoningEffort
      ?? "";
  const sandboxMode = typeof metadata.sandboxMode === "string" ? metadata.sandboxMode : "";
  const approvalPolicy = typeof metadata.sandboxApprovalPolicy === "string" ? metadata.sandboxApprovalPolicy
    : typeof metadata.approvalPolicy === "string" ? metadata.approvalPolicy : "";
  selectedAutonomyPreset.value = sandboxMode === "danger-full-access" && approvalPolicy === "never" ? "full"
    : sandboxMode === "read-only" || approvalPolicy === "untrusted" ? "read-only"
      : sandboxMode === "workspace-write" && metadata.sandboxAllowNetwork === true ? "auto"
        : sandboxMode === "workspace-write" || approvalPolicy === "on-request" ? "ask" : "inherit";
}

async function initializeConversation() {
  if (!auracoderId.value || !workspaceId.value || !threadId.value || !auracoderConnectionManager.getState(auracoderId.value).peerOnline || initializing) return;
  initializing = true;
  try {
    if (!projectStore.getThreads(auracoderId.value, workspaceId.value).length) await projectStore.load(auracoderId.value, workspaceId.value);
    // 重构初版让引擎读取失败直接终止会话打开；保留原语句以便追溯。
    /* await workspaceStore.loadEngines(auracoderId.value); */
    try {
      await workspaceStore.loadEngines(auracoderId.value);
    } catch (error) {
      // 消息和会话不依赖引擎列表；引擎稍后恢复即可。
      console.warn("加载引擎列表失败", error);
    }
    applyThreadDefaults(thread.value);
    // 重构初版会回退到全量模型首项，保留原语句以便追溯。
    /* if (!selectedModelId.value && models.value[0]) selectedModelId.value = models.value[0].id; */
    if (!selectedModelId.value && visibleModels.value[0]) selectedModelId.value = visibleModels.value[0].id;
    if (!selectedReasoningEffort.value) selectedReasoningEffort.value = selectedModel.value?.supportedReasoningEfforts[0]?.reasoningEffort || "";
    await conversationStore.open(auracoderId.value, threadId.value);
    await nextTick();
    if (hasRestoredScrollPosition && typeof restoredScrollTop === "number") {
      // 原生 scroll-view 需要等消息节点出现后再次接收位置；静态属性保证无滚动动画。
      scrollTop.value = restoredScrollTop;
      lastScrollTop = restoredScrollTop;
    } else {
      scrollToNewest();
    }
  } catch (error) {
    uni.showToast({ title: error instanceof Error ? error.message : "无法加载会话", icon: "none" });
  } finally {
    initializing = false;
  }
}

/*
async function loadOlder() {
  if (!conversation.value) return;
  try {
    await conversationStore.loadOlder(auracoderId.value, threadId.value);
  } catch (error) {
    uni.showToast({ title: error instanceof Error ? error.message : "无法加载历史消息", icon: "none" });
  }
}
*/
/*
已恢复分页锚点加载；保留旧存根以便追溯。
async function loadOlder() {
  // 旧模板入口仍会被编译；完整取数后该调用不会请求更多消息。
  await conversationStore.loadOlder(auracoderId.value, threadId.value);
}
*/
async function loadOlder() {
  const current = conversation.value;
  if (!current || !current.nextCursor || current.loadingOlder) return;
  // 记录前插前最旧一条作为滚动锚点，加载完成后把视口钉回它。
  const anchorId = current.messages[0]?.id;
  suppressUnseenHint = true;
  try {
    await conversationStore.loadOlder(auracoderId.value, threadId.value);
    if (anchorId) {
      await nextTick();
      scrollIntoViewId.value = "";
      await nextTick();
      scrollIntoViewId.value = `msg-${anchorId}`;
    }
  } catch (error) {
    uni.showToast({ title: error instanceof Error ? error.message : "无法加载历史消息", icon: "none" });
  } finally {
    suppressUnseenHint = false;
  }
}

/* 重构初版可选择隐藏模型，并给不支持的模型写入 high；保留原实现以便追溯。
function chooseModel(modelId: string) {
  const model = models.value.find((item) => item.id === modelId);
  if (!model) return;
  selectedModelId.value = model.id;
  selectedReasoningEffort.value = model.defaultReasoningEffort || model.supportedReasoningEfforts[0]?.reasoningEffort || "high";
}
*/
function chooseModel(modelId: string) {
  const model = visibleModels.value.find((item) => item.id === modelId);
  if (!model) return;
  selectedModelId.value = model.id;
  selectedReasoningEffort.value = model.supportedReasoningEfforts.find((item) => item.reasoningEffort === model.defaultReasoningEffort)?.reasoningEffort
    ?? model.supportedReasoningEfforts[0]?.reasoningEffort
    ?? "";
}

function chooseReasoningEffort(reasoningEffort: string) {
  if (!efforts.value.some((item) => item.reasoningEffort === reasoningEffort)) return;
  selectedReasoningEffort.value = reasoningEffort;
}

function chooseAutonomy(preset?: AutonomyPreset) {
  if (preset) {
    permissionSaving.value = true;
    void auracoderConnectionManager.request<Thread>(auracoderId.value, "thread.set_autonomy_preset", { thread_id: threadId.value, preset }).then((updated) => {
      projectStore.upsert(auracoderId.value, updated);
      selectedAutonomyPreset.value = preset;
      permissionPickerOpen.value = false;
    }).catch((error) => {
      uni.showToast({ title: error instanceof Error ? error.message : "无法更新权限", icon: "none" });
    }).finally(() => {
      permissionSaving.value = false;
    });
    return;
  }
  /* 重构初版使用系统 ActionSheet，保留其实现以便追溯。
  const options: Array<{ id: AutonomyPreset; label: string }> = [
    { id: "inherit", label: "跟随默认权限" }, { id: "read-only", label: "只读权限" }, { id: "ask", label: "标准访问权限" }, { id: "auto", label: "工作区自动权限" }, { id: "full", label: "完全访问权限" },
  ];
  uni.showActionSheet({ itemList: options.map((item) => item.label), success: async (result) => {
    const option = options[result.tapIndex];
    if (!option) return;
    try {
      const updated = await auracoderConnectionManager.request<Thread>(auracoderId.value, "thread.set_autonomy_preset", { thread_id: threadId.value, preset: option.id });
      projectStore.upsert(auracoderId.value, updated);
      selectedAutonomyPreset.value = option.id;
    } catch (error) {
      uni.showToast({ title: error instanceof Error ? error.message : "无法更新权限", icon: "none" });
    }
  } });
  */
}

function attachmentCounts() {
  const attachments = conversation.value?.attachments || [];
  return {
    // 从图片入口选择的数量，不按 MIME 推断。
    images: attachments.filter((item) => item.source === "image").length,
    // 从文件入口选择的数量，不按 MIME 推断。
    files: attachments.filter((item) => item.source === "file").length,
  };
}

async function queueAttachment(filePath: string, fallbackName: string, fallbackMimeType: string, source: ChatAttachmentSource) {
  const state = conversation.value;
  if (!state || !filePath) return;
  const counts = attachmentCounts();
  if (state.attachments.length >= 10 || (source === "image" && counts.images >= 5) || (source === "file" && counts.files >= 5)) {
    uni.showToast({ title: source === "image" ? "最多选择 5 张图片" : "最多选择 5 个附件", icon: "none" });
    return;
  }
  const localId = `mobile-${Date.now()}-${Math.random().toString(16).slice(2)}`;
  // 先占用一个来源类别名额，避免相册/系统多选的并发回调超出上限。
  state.attachments.push({
    // 手机端本地附件标识。
    id: localId,
    // 元数据异步查询完成前使用选择器回退名称。
    fileName: fallbackName,
    // 发送批次上传前为空。
    filePath: "",
    // 供发送批次读取的本地路径或 content URI。
    localPath: filePath,
    // 选择入口来源。
    source,
    // 选择阶段尚未读取大小。
    sizeBytes: 0,
    // 选择阶段使用选择器回退 MIME。
    mimeType: fallbackMimeType,
    // 选择阶段不得显示为上传中。
    uploading: false,
  });
  try {
    // 选择阶段只查询元数据，不读取或上传文件内容。
    const metadata = await inspectLocalFile(filePath, fallbackName, fallbackMimeType);
    const index = state.attachments.findIndex((item) => item.id === localId);
    if (index >= 0) state.attachments.splice(index, 1, {
      // 保留本地附件标识。
      id: localId,
      // 选择器返回或元数据查询得到的名称。
      fileName: metadata.fileName,
      // 发送批次上传前为空。
      filePath: "",
      // 供发送批次读取的本地路径或 content URI。
      localPath: filePath,
      // 选择入口来源。
      source,
      // 选择阶段可用的大小。
      sizeBytes: metadata.sizeBytes,
      // 选择阶段可用的 MIME。
      mimeType: metadata.mimeType,
      // 选择阶段不得显示为上传中。
      uploading: false,
    });
  } catch (error) {
    const index = state.attachments.findIndex((item) => item.id === localId);
    if (index >= 0) state.attachments.splice(index, 1);
    uni.showToast({ title: error instanceof Error ? error.message : "无法读取所选附件", icon: "none" });
  }
}

function chooseAttachment() {
  if (!conversation.value || pendingBatch.value || !auracoderConnectionManager.getState(auracoderId.value).peerOnline) return;
  const counts = attachmentCounts();
  if (conversation.value.attachments.length >= 10) {
    uni.showToast({ title: "附件总数最多 10 个", icon: "none" });
    return;
  }
  uni.showActionSheet({ itemList: ["拍照", "从相册选择", "选择文件"], success: (choice) => {
    if (choice.tapIndex === 2) {
      const remainingFiles = Math.min(5 - counts.files, 10 - conversation.value!.attachments.length);
      if (remainingFiles <= 0) {
        uni.showToast({ title: "最多选择 5 个普通附件", icon: "none" });
        return;
      }
      const chooseFile = (uni as unknown as { chooseFile?: (options: Record<string, unknown>) => void }).chooseFile;
      const openAndroidPicker = () => void chooseAndroidFiles(remainingFiles).then((files) => files.forEach((file) => void queueAttachment(
        file.filePath,
        file.fileName,
        file.mimeType,
        "file",
      ))).catch((error) => uni.showToast({ title: error instanceof Error ? error.message : "无法打开系统文件选择器", icon: "none" }));
      if (chooseFile) {
        chooseFile({ count: remainingFiles, type: "all", success: (result: any) => {
          const paths = Array.isArray(result.tempFilePaths) ? result.tempFilePaths : result.tempFilePaths ? [result.tempFilePaths] : [];
          const files = Array.isArray(result.tempFiles) ? result.tempFiles : result.tempFiles ? [result.tempFiles] : [];
          paths.slice(0, remainingFiles).forEach((path: string, index: number) => void queueAttachment(
            String(path),
            String(files[index]?.name || `附件-${index + 1}`),
            String(files[index]?.type || "application/octet-stream"),
            "file",
          ));
        }, fail: () => {
          // chooseFile 运行时失败时，App-Plus 继续使用同一系统多选器。
          openAndroidPicker();
        } });
      } else {
        openAndroidPicker();
      }
      return;
    }
    const remainingImages = Math.min(5 - counts.images, 10 - conversation.value!.attachments.length);
    if (remainingImages <= 0) {
      uni.showToast({ title: "最多选择 5 张图片", icon: "none" });
      return;
    }
    uni.chooseImage({
      // 拍照一次一张，相册按剩余额度多选。
      count: choice.tapIndex === 0 ? 1 : remainingImages,
      sizeType: ["original"],
      sourceType: choice.tapIndex === 0 ? ["camera"] : ["album"],
      success: (result) => {
        const paths = Array.isArray(result.tempFilePaths) ? result.tempFilePaths : [result.tempFilePaths];
        paths.filter(Boolean).slice(0, remainingImages).forEach((path: string, index: number) => void queueAttachment(
          String(path),
          `图片-${index + 1}.jpg`,
          "image/jpeg",
          "image",
        ));
      },
      fail: (error) => {
        if (!String(error.errMsg || "").toLowerCase().includes("cancel")) uni.showToast({ title: "无法选择附件，请检查权限", icon: "none" });
      },
    });
  } });
}

function removeAttachment(id: string) {
  if (!conversation.value || pendingBatch.value) return;
  cancelledAttachments.add(id);
  const index = conversation.value.attachments.findIndex((item) => item.id === id);
  if (index >= 0) conversation.value.attachments.splice(index, 1);
}

async function sendMessage() {
  if (!conversation.value || activeTurn.value || pendingBatch.value) return;
  try {
    await conversationStore.send(auracoderId.value, threadId.value, selectedModelId.value, selectedReasoningEffort.value);
    await nextTick();
    scrollToNewest();
  } catch (error) {
    uni.showToast({ title: error instanceof Error ? error.message : "发送失败", icon: "none" });
  }
}

async function retryBatch() {
  if (!pendingBatch.value) return;
  try {
    await conversationStore.retryBatch(auracoderId.value, threadId.value);
  } catch (error) {
    uni.showToast({ title: error instanceof Error ? error.message : "重试发送失败", icon: "none" });
  }
}

async function abortBatch() {
  if (!pendingBatch.value) return;
  try {
    await conversationStore.abortBatch(auracoderId.value, threadId.value);
  } catch (error) {
    uni.showToast({ title: error instanceof Error ? error.message : "取消发送失败", icon: "none" });
  }
}

async function stopTurn() {
  try {
    await auracoderConnectionManager.request(auracoderId.value, "turn.stop", { thread_id: threadId.value });
  } catch (error) {
    uni.showToast({ title: error instanceof Error ? error.message : "无法停止执行", icon: "none" });
  }
}

onLoad((query) => {
  const options = query || {};
  auracoderId.value = String(options.auracoderId || "");
  workspaceId.value = String(options.workspaceId || "");
  threadId.value = String(options.threadId || "");
  if (!auracoderDeviceStore.getDevice(auracoderId.value) || !workspaceId.value || !threadId.value) {
    uni.showToast({ title: "会话参数无效", icon: "none" });
    uni.navigateBack();
    return;
  }
  const viewportKey = conversationViewportKey();
  hasRestoredScrollPosition = conversationScrollTopMap.has(viewportKey);
  restoredScrollTop = hasRestoredScrollPosition ? conversationScrollTopMap.get(viewportKey) : undefined;
  if (hasRestoredScrollPosition && typeof restoredScrollTop === "number") {
    // Map 命中时直接恢复原始位置；0 不能被当成“没有记录”。
    scrollTop.value = restoredScrollTop;
    lastScrollTop = restoredScrollTop;
    isAtNewest.value = false;
    // 离开期间收到的新消息已登记在未读状态中，重新进入时继续显示向下提示。
    hasUnseenMessages.value = conversationStore.getUnread(auracoderId.value, threadId.value) !== null;
  }
  // 设备级事件不随页面订阅；页面只登记当前可见会话，便于区分当前会话和未读会话。
  conversationStore.setActive(auracoderId.value, threadId.value, isAtNewest.value);
  unsubscribeState = auracoderConnectionManager.subscribeState((changedAuraCoderId, state, previous) => {
    if (changedAuraCoderId === auracoderId.value && !previous.peerOnline && state.peerOnline) void initializeConversation();
  });
  if (!auracoderConnectionManager.getState(auracoderId.value).relayConnected) auracoderConnectionManager.connect(auracoderId.value);
  void initializeConversation();
});

onShow(() => {
  if (auracoderId.value && threadId.value) conversationStore.setActive(auracoderId.value, threadId.value, isAtNewest.value);
});

onUnload(() => {
  unsubscribeState?.();
  unsubscribeState = undefined;
  if (auracoderId.value && threadId.value) {
    // onUnload 是最终边界：非底部保存最后一次滚动事件位置，底部删除恢复记录。
    const viewportKey = conversationViewportKey();
    if (isAtNewest.value) conversationScrollTopMap.delete(viewportKey);
    else conversationScrollTopMap.set(viewportKey, lastScrollTop);
    void conversationStore.close(auracoderId.value, threadId.value);
  }
});
</script>

<template>
  <!-- 旧版模板保留在不可执行分支中，避免嵌套 HTML 注释破坏 UniApp 编译。 -->
  <!-- @vue-ignore -->
  <view v-if="false && conversation" aria-hidden="true">
  <view class="conversation-page">
    <view class="conversation-meta"><button class="meta-chip" @tap="chooseModel">{{ selectedModel?.displayName || selectedModelId || '模型' }}</button><button class="meta-chip" @tap="chooseReasoningEffort">{{ selectedReasoningEffort || '思考强度' }}</button><button class="meta-chip" @tap="chooseAutonomy">{{ selectedAutonomyPreset === 'full' ? '完全访问' : selectedAutonomyPreset === 'auto' ? '自动权限' : selectedAutonomyPreset === 'read-only' ? '只读' : selectedAutonomyPreset === 'ask' ? '标准权限' : '跟随权限' }}</button></view>
    <view v-if="!auracoderConnectionManager.getState(auracoderId).peerOnline" class="offline-banner">桌面 AuraCoder 当前离线，消息与草稿已保留。</view>
    <view v-if="!conversation && !auracoderConnectionManager.getState(auracoderId).peerOnline" class="empty-state conversation-waiting"><view class="loader"></view><text>正在等待桌面 AuraCoder 连接…</text></view>
    <!-- 未经用户确认的滚动加载改动，保留记录但不启用。 -->
    <!-- <scroll-view class="chat-scroll" scroll-y :scroll-top="scrollTop" :scroll-with-animation="false" @scrolltoupper="loadOlder"> -->
    <scroll-view class="chat-scroll" scroll-y :scroll-top="scrollTop" :scroll-with-animation="false">
      <view class="chat-content">
        <!-- 未经用户确认的滚动加载改动：<button v-if="conversation?.nextCursor" class="load-older" :disabled="conversation.loadingOlder" @tap="loadOlder">{{ conversation.loadingOlder ? '正在加载…' : '加载更早消息' }}</button> -->
        <button v-if="conversation?.nextCursor" class="load-older" :disabled="conversation.loadingOlder" @tap="loadOlder">{{ conversation.loadingOlder ? '正在加载…' : '加载更早消息' }}</button>
        <view v-if="conversation?.loading && !conversation.messages.length" class="empty-state"><view class="loader"></view><text>正在加载消息…</text></view>
        <view v-for="message in conversation?.messages || []" :key="message.id" class="message" :class="message.role">
          <text class="message-role">{{ message.role === 'user' ? '你' : 'AuraCoder' }}</text>
          <view v-if="message.attachments?.length" class="message-attachments">
            <view class="message-images">
              <template v-for="attachment in message.attachments" :key="`image-${attachment.id}`">
                <image v-if="attachment.source === 'image'" class="message-image" :src="attachment.localPath || attachment.filePath" mode="aspectFill" :aria-label="attachment.fileName" />
              </template>
            </view>
            <view class="message-files">
              <template v-for="attachment in message.attachments" :key="`file-${attachment.id}`">
                <view v-if="attachment.source === 'file'" class="message-file-item"><text class="message-file-name">{{ attachment.fileName }}</text><text class="message-file-meta">{{ attachment.mimeType || '附件' }}{{ attachment.sizeBytes ? ` · ${attachment.sizeBytes} B` : '' }}</text></view>
              </template>
            </view>
          </view>
          <view class="markdown"><MessageContent :message="message"/></view>
          <text v-if="message.status === 'streaming'" class="streaming">正在生成</text>
        </view>
      </view>
    </scroll-view>
    <view class="composer">
      <scroll-view v-if="conversation?.attachments.length" class="attachment-strip" scroll-x><view class="attachment-list"><view v-for="attachment in conversation.attachments" :key="attachment.id" class="attachment-item"><view><text>{{ attachment.fileName }}</text><text>{{ attachment.uploading ? '上传中' : attachment.failed ? attachment.error || '上传失败' : '已就绪' }}</text></view><button class="attachment-remove" @tap="removeAttachment(attachment.id)">{{ attachment.uploading ? '取消' : '移除' }}</button></view></view></scroll-view>
      <view class="composer-row"><button class="attachment-button" :disabled="!auracoderConnectionManager.getState(auracoderId).peerOnline || attachmentUploading" @tap="chooseAttachment">＋</button><textarea v-model="conversation!.draft" class="composer-input" auto-height :maxlength="100000" placeholder="给 AuraCoder 发送消息" placeholder-class="input-placeholder" :disabled="!auracoderConnectionManager.getState(auracoderId).peerOnline"></textarea><button class="send" :class="{ stop: activeTurn }" :disabled="activeTurn ? false : !conversation?.draft.trim() && !conversation?.attachments.length" @tap="activeTurn ? stopTurn() : sendMessage()">{{ activeTurn ? '停止' : '发送' }}</button></view></view>
  </view>
  </view>
  <view class="conversation-page">
    <view v-if="!auracoderConnectionManager.getState(auracoderId).peerOnline" class="offline-banner">桌面 AuraCoder 当前离线，消息与草稿已保留。</view>
    <view v-if="!conversation && !auracoderConnectionManager.getState(auracoderId).peerOnline" class="empty-state conversation-waiting"><view class="loader"></view><text>正在等待桌面 AuraCoder 连接…</text></view>
    <scroll-view class="chat-scroll" scroll-y :scroll-top="scrollTop" :scroll-with-animation="false" :scroll-into-view="scrollIntoViewId" @scroll="handleChatScroll" @scrolltolower="handleScrollToLower">
      <view class="chat-content">
        <!-- 存在更早历史时显示分页入口。 -->
        <button v-if="conversation?.nextCursor" class="load-older" :disabled="conversation.loadingOlder" @tap="loadOlder">{{ conversation.loadingOlder ? '正在加载…' : '加载更早消息' }}</button>
        <view v-if="conversation?.loading && !conversation.messages.length" class="empty-state"><view class="loader"></view><text>正在加载消息…</text></view>
        <view v-for="message in conversation?.messages || []" :key="message.id" :id="'msg-' + message.id" class="message" :class="message.role">
          <text class="message-role">{{ message.role === 'user' ? '你' : 'AuraCoder' }}</text>
          <view class="message-body">
            <view v-if="message.attachments?.length" class="message-attachments">
              <view class="message-images">
                <template v-for="attachment in message.attachments" :key="`image-${attachment.id}`">
                  <!-- 原生 previewImage 对 data: 地址未在真机打开预览，保留旧事件绑定以便追溯。 -->
                  <!-- <image v-if="attachment.source === 'image' && (attachment.previewUrl || attachment.localPath || attachment.filePath)" class="message-image" :src="attachment.previewUrl || attachment.localPath || attachment.filePath" mode="aspectFill" :aria-label="attachment.fileName" @tap.stop="previewImage({ current: attachment.previewUrl || attachment.localPath || attachment.filePath, urls: [attachment.previewUrl || attachment.localPath || attachment.filePath] })" /> -->
                  <view v-if="attachment.source === 'image' && (attachment.previewUrl || attachment.localPath || attachment.filePath)" class="message-image-trigger" @tap="previewImageSource = attachment.previewUrl || attachment.localPath || attachment.filePath">
                    <image class="message-image" :src="attachment.previewUrl || attachment.localPath || attachment.filePath" mode="aspectFill" :aria-label="attachment.fileName" />
                  </view>
                </template>
              </view>
              <view class="message-files">
                <template v-for="attachment in message.attachments" :key="`file-${attachment.id}`">
                  <view v-if="attachment.source === 'file'" class="message-file-item"><text class="message-file-name">{{ attachment.fileName }}</text><text class="message-file-meta">{{ attachment.mimeType || '附件' }}{{ attachment.sizeBytes ? ` · ${attachment.sizeBytes} B` : '' }}</text></view>
                </template>
              </view>
            </view>
            <view class="markdown"><MessageContent :message="message"/></view>
          </view>
          <text v-if="message.status === 'streaming'" class="streaming">正在生成</text>
        </view>
      </view>
    </scroll-view>
    <button v-if="hasUnseenMessages" class="new-message-hint" hover-class="none" aria-label="查看新消息" @tap="scrollToNewest">↓</button>
    <view v-if="conversation" class="composer">
      <!-- 重构初版以系统 ActionSheet 呈现模型和权限，保留其触发写法以便追溯。 -->
      <!-- <view class="composer-meta"><button class="composer-chip composer-chip-button" hover-class="none" @tap="chooseModel">{{ runtimeLabel }}</button><button class="composer-chip composer-chip-button" hover-class="none" @tap="chooseAutonomy">{{ accessLabel }}</button></view> -->
      <!-- 真机上切换表达式会在 tap 结束前被再次触发，面板随即关闭；保留原写法以便追溯。 -->
      <!-- <view class="composer-meta"><button class="composer-chip composer-chip-button" hover-class="none" @tap="permissionPickerOpen = false; runtimePickerOpen = !runtimePickerOpen">{{ runtimeLabel }}</button><button class="composer-chip composer-chip-button" hover-class="none" @tap="runtimePickerOpen = false; permissionPickerOpen = !permissionPickerOpen">{{ accessLabel }}</button></view> -->
      <view class="composer-meta"><button class="composer-chip composer-chip-button" hover-class="none" @tap.stop="runtimePickerOpen = true">{{ runtimeLabel }}</button><button class="composer-chip composer-chip-button" hover-class="none" @tap.stop="permissionPickerOpen = true">{{ accessLabel }}</button></view>
      <view v-if="pendingBatch" class="batch-status" :class="{ failed: pendingBatch.status === 'failed' }"><text>{{ pendingBatch.status === 'failed' ? `本批发送失败：${pendingBatch.error || '请重试或取消'}` : pendingBatch.status === 'sending' ? '正在提交本批…' : '正在上传本批附件…' }}</text><view class="batch-status-actions"><button v-if="pendingBatch.status === 'failed'" class="batch-retry" @tap="retryBatch">重试本批</button><button class="batch-abort" @tap="abortBatch">取消本批</button></view></view>
      <scroll-view v-if="displayedAttachments.length" class="composer-attachments" scroll-x><view class="composer-attachment-track"><view v-for="attachment in displayedAttachments" :key="attachment.id" class="composer-attachment"><view class="attachment-thumb">{{ attachment.source === 'image' ? '图' : '文' }}</view><view class="attachment-copy"><text>{{ attachment.fileName }}</text><text>{{ pendingBatch ? (attachment.uploading ? '正在上传…' : attachment.failed ? attachment.error || '上传失败' : attachment.attachmentKey ? '已上传' : '待上传') : '已选择' }}</text></view><button class="attachment-remove" aria-label="移除附件" :disabled="Boolean(pendingBatch)" @tap="removeAttachment(attachment.id)">×</button></view></view></scroll-view>
      <view class="composer-row"><button class="attachment-button" aria-label="选择附件" :disabled="!auracoderConnectionManager.getState(auracoderId).peerOnline || Boolean(pendingBatch) || conversation.attachments.length >= 10" @tap="chooseAttachment">＋</button><view class="composer-field"><textarea v-model="conversation.draft" class="composer-input" auto-height :disabled="!auracoderConnectionManager.getState(auracoderId).peerOnline || activeTurn || Boolean(pendingBatch)" :maxlength="-1" confirm-type="send" :placeholder="composerPlaceholder" @confirm="sendMessage"></textarea><button class="composer-action" :class="{ ready: hasComposerContent, stop: activeTurn }" :disabled="!activeTurn && (!auracoderConnectionManager.getState(auracoderId).peerOnline || Boolean(pendingBatch) || !hasComposerContent)" @tap="activeTurn ? stopTurn() : sendMessage()"><text v-if="activeTurn" class="stop-icon">■</text><uni-icons v-else-if="hasComposerContent" class="composer-send-icon" type="arrowthinup" :size="25" color="#ffffff"/><view v-else class="waveform-icon"><text></text><text></text><text></text><text></text></view></button></view></view>
    </view>
    <!-- 重构初版将全部模型和协议强度值原样展示；保留原结构以便追溯。
    <view v-if="runtimePickerOpen" class="mobile-picker-backdrop" @tap="runtimePickerOpen = false"><view class="mobile-picker" @tap.stop><view class="mobile-picker-header"><text>模型与思考强度</text><button hover-class="none" @tap="runtimePickerOpen = false">完成</button></view><text class="mobile-picker-section-title">模型</text><scroll-view class="mobile-picker-list" scroll-y><button v-for="model in models" :key="model.id" class="mobile-picker-option" :class="{ selected: selectedModelId === model.id }" hover-class="none" @tap="chooseModel(model.id)"><view><text>{{ model.displayName || model.id }}</text><text>{{ model.description }}</text></view><text>{{ selectedModelId === model.id ? '✓' : '' }}</text></button></scroll-view><text class="mobile-picker-section-title">思考强度</text><view class="effort-options"><button v-for="effort in efforts" :key="effort.reasoningEffort" class="effort-option" :class="{ selected: selectedReasoningEffort === effort.reasoningEffort }" hover-class="none" @tap="chooseReasoningEffort(effort.reasoningEffort)">{{ effort.reasoningEffort }}</button></view></view></view> -->
    <view v-if="runtimePickerOpen" class="mobile-picker-backdrop" @tap="runtimePickerOpen = false">
      <view class="mobile-picker" @tap.stop>
        <view class="mobile-picker-header"><text>模型与思考强度</text><button hover-class="none" @tap="runtimePickerOpen = false">完成</button></view>
        <text class="mobile-picker-section-title">模型</text>
        <scroll-view class="mobile-picker-list" scroll-y>
          <button v-for="model in visibleModels" :key="model.id" class="mobile-picker-option" :class="{ selected: selectedModelId === model.id }" hover-class="none" @tap="chooseModel(model.id)"><view><text>{{ model.displayName || model.id }}</text><text>{{ model.description }}</text></view><text>{{ selectedModelId === model.id ? '✓' : '' }}</text></button>
        </scroll-view>
        <text class="mobile-picker-section-title">思考强度</text>
        <view class="effort-options">
          <button v-for="effort in efforts" :key="effort.reasoningEffort" class="effort-option" :class="{ selected: selectedReasoningEffort === effort.reasoningEffort }" hover-class="none" @tap="chooseReasoningEffort(effort.reasoningEffort)">{{ formatReasoningEffort(effort.reasoningEffort) }}</button>
        </view>
      </view>
    </view>
    <view v-if="permissionPickerOpen" class="mobile-picker-backdrop" @tap="permissionPickerOpen = false"><view class="mobile-picker permission-picker" @tap.stop><view class="mobile-picker-header"><text>访问权限</text><button hover-class="none" @tap="permissionPickerOpen = false">完成</button></view><button v-for="option in autonomyOptions" :key="option.id" class="mobile-picker-option" :class="{ selected: selectedAutonomyPreset === option.id }" hover-class="none" :disabled="permissionSaving" @tap="chooseAutonomy(option.id)"><view><text>{{ option.label }}</text><text>{{ option.description }}</text></view><text>{{ selectedAutonomyPreset === option.id ? '✓' : '' }}</text></button></view></view>
    <view v-if="previewImageSource" class="message-image-preview-backdrop" @tap="previewImageSource = ''">
      <image class="message-image-preview" :src="previewImageSource" mode="aspectFit" @tap.stop />
      <button class="message-image-preview-close" hover-class="none" aria-label="关闭图片预览" @tap.stop="previewImageSource = ''">×</button>
    </view>
  </view>
</template>

<style scoped>
.conversation-page { display: grid; height: 100vh; grid-template-rows: auto auto minmax(0, 1fr) auto; background: var(--bg); }.conversation-meta { display: flex; padding: 8px 12px; gap: 7px; overflow-x: auto; border-bottom: 1px solid var(--line); white-space: nowrap; }.meta-chip { min-height: 28px; padding: 0 10px; overflow: hidden; border-radius: 999px; color: var(--muted); background: var(--surface); font-size: 10px; text-overflow: ellipsis; white-space: nowrap; }.offline-banner { padding: 7px 12px; color: #f7c06e; background: rgba(247,192,110,.1); font-size: 10px; text-align: center; }.chat-scroll { height: 100%; }.chat-content { padding: 16px 13px 22px; }.markdown { display: block; padding: 11px 13px; border: 1px solid var(--line); border-radius: 5px 15px 15px 15px; background: var(--surface); font-size: 13px; line-height: 1.65; }.message.user .markdown { border-color: rgba(70, 211, 154, .18); border-radius: 15px 5px 15px 15px; background: rgba(38, 117, 87, .28); }.composer { display: flex; padding: 8px 10px calc(10px + env(safe-area-inset-bottom)); flex-direction: column; gap: 8px; border-top: 1px solid var(--line); }.composer-row { display: grid; grid-template-columns: 42px minmax(0, 1fr) 54px; align-items: end; gap: 7px; }.attachment-button { display: flex; width: 42px; height: 42px; align-items: center; justify-content: center; border-radius: 13px; color: var(--text); background: var(--raised); font-size: 24px; font-weight: 300; }.composer-input { width: 100%; min-height: 42px; max-height: 126px; padding: 10px 11px; border: 1px solid var(--line); border-radius: 13px; color: var(--text); background: var(--surface); font-size: 13px; }.send { width: 54px; height: 42px; border-radius: 13px; color: #07140f; background: var(--accent); font-size: 11px; font-weight: 800; }.send.stop { color: #fff; background: var(--danger); }.input-placeholder { color: #6c7686; }.attachment-strip { width: 100%; white-space: nowrap; }.attachment-list { display: inline-flex; gap: 7px; }.attachment-item { display: inline-flex; max-width: 210px; padding: 7px 8px; align-items: center; gap: 7px; border: 1px solid var(--line); border-radius: 9px; background: var(--surface); }.attachment-item view { min-width: 0; }.attachment-item text { display: block; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }.attachment-item text:first-child { font-size: 10px; }.attachment-item text:last-child { margin-top: 3px; color: var(--muted); font-size: 8px; }.attachment-remove { padding: 3px 5px; border-radius: 5px; color: var(--muted); background: rgba(255,255,255,.06); font-size: 9px; }.message { max-width: 92%; margin-bottom: 18px; }.message.user { margin-left: auto; }.message-role { display: block; margin: 0 7px 6px; color: var(--muted); font-size: 9px; font-weight: 700; }.message.user .message-role { text-align: right; } /* 本地消息附件位于正文上方，图片和普通文件分别排列。 */ .message-attachments { display: flex; margin-bottom: 7px; flex-direction: column; gap: 7px; }.message-images { display: flex; flex-wrap: wrap; gap: 6px; }.message-image-trigger { width: 104px; height: 104px; }.message-image { width: 104px; height: 104px; border-radius: 9px; background: var(--surface); }.message-files { display: flex; flex-direction: column; gap: 5px; }.message-file-item { display: flex; min-width: 0; padding: 7px 9px; flex-direction: column; border: 1px solid var(--line); border-radius: 8px; background: var(--surface); }.message-file-name { overflow: hidden; color: var(--text); font-size: 10px; text-overflow: ellipsis; white-space: nowrap; }.message-file-meta { margin-top: 3px; color: var(--muted); font-size: 8px; }.streaming { display: block; margin: 5px 7px 0; color: var(--accent); font-size: 9px; }.load-older { width: 128px; min-height: 34px; margin: 0 auto 18px; color: var(--muted); font-size: 10px; }
.conversation-page { display: flex; height: 100vh; flex-direction: column; background: var(--bg); }
.chat-scroll { height: auto; min-height: 0; flex: 1; }
.chat-content { min-height: 100%; }
.composer { flex-shrink: 0; background: var(--bg); }
/* 重构初版的局部样式覆盖了既有输入区控件尺寸，保留以便追溯。 */
/* .composer-meta { display: flex; gap: 7px; overflow-x: auto; white-space: nowrap; } */
/* .composer-chip { min-height: 28px; padding: 0 10px; border: 1px solid var(--line); border-radius: 999px; color: var(--muted); background: var(--surface); font-size: 10px; } */
/* .composer-chip-button { max-width: 70%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; } */
.composer-attachments { width: 100%; white-space: nowrap; }
.composer-attachment-track { display: inline-flex; gap: 7px; }
.composer-attachment { display: inline-flex; max-width: 210px; padding: 7px 8px; align-items: center; gap: 7px; border: 1px solid var(--line); border-radius: 9px; background: var(--surface); }
.attachment-thumb { display: flex; width: 26px; height: 26px; flex-shrink: 0; align-items: center; justify-content: center; border-radius: 7px; color: var(--accent); background: var(--soft); font-size: 10px; font-weight: 700; }
.attachment-copy { min-width: 0; }.attachment-copy text { display: block; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }.attachment-copy text:first-child { font-size: 10px; }.attachment-copy text:last-child { margin-top: 3px; color: var(--muted); font-size: 8px; }
.attachment-remove { display: flex; min-width: 22px; min-height: 22px; padding: 0; align-items: center; justify-content: center; border-radius: 5px; color: var(--muted); background: rgba(255,255,255,.06); font-size: 12px; }
.composer-row { display: flex; grid-template-columns: none; align-items: flex-end; gap: 8px; }
.attachment-button { flex-shrink: 0; }.composer-field { position: relative; min-width: 0; flex: 1; }
.composer-input { box-sizing: border-box; padding-right: 54px; }
.composer-action { position: absolute; right: 5px; bottom: 5px; display: flex; width: 32px; height: 32px; padding: 0; align-items: center; justify-content: center; border-radius: 10px; color: #07140f; background: #56606f; }
.composer-action.ready { background: var(--accent); }.composer-action.stop { color: #fff; background: var(--danger); }.stop-icon { font-size: 12px; }
.waveform-icon { display: flex; height: 14px; align-items: center; gap: 2px; }.waveform-icon text { display: block; width: 2px; border-radius: 2px; background: #09150f; }.waveform-icon text:nth-child(1), .waveform-icon text:nth-child(4) { height: 5px; }.waveform-icon text:nth-child(2) { height: 12px; }.waveform-icon text:nth-child(3) { height: 8px; }
.mobile-picker-backdrop { position: fixed; z-index: 1000; inset: 0; display: flex; align-items: flex-end; background: rgba(0, 0, 0, .58); }.mobile-picker { box-sizing: border-box; width: 100%; max-height: 72vh; padding: 18px 18px calc(18px + env(safe-area-inset-bottom)); overflow-y: auto; border: 1px solid var(--line); border-radius: 24px 24px 0 0; background: var(--surface); }.mobile-picker-header { display: flex; margin-bottom: 16px; align-items: center; justify-content: space-between; }.mobile-picker-header > text { color: var(--text); font-size: 18px; font-weight: 700; }.mobile-picker-header button { margin: 0; padding: 6px 10px; color: var(--accent); background: transparent; font-size: 14px; line-height: 1.2; }.mobile-picker-section-title { display: block; margin: 14px 0 8px; color: var(--muted); font-size: 12px; }.mobile-picker-list { max-height: 270px; }.mobile-picker-option { display: flex; width: 100%; min-height: 58px; box-sizing: border-box; margin: 0 0 8px; padding: 10px 14px; align-items: center; justify-content: space-between; border: 1px solid var(--line); border-radius: 14px; color: var(--text); background: var(--raised); text-align: left; line-height: 1.25; }.mobile-picker-option.selected { border-color: rgba(74, 213, 157, .55); background: rgba(74, 213, 157, .1); }.mobile-picker-option > view { display: flex; min-width: 0; flex: 1; flex-direction: column; gap: 4px; }.mobile-picker-option > view > text:first-child { color: var(--text); font-size: 14px; font-weight: 650; }.mobile-picker-option > view > text:last-child { color: var(--muted); font-size: 12px; }.mobile-picker-option > text { margin-left: 12px; color: var(--accent); font-size: 18px; }.effort-options { display: flex; flex-wrap: wrap; gap: 8px; }.effort-option { min-width: 62px; min-height: 34px; padding: 0 12px; border: 1px solid var(--line); border-radius: 17px; color: var(--muted); background: var(--raised); font-size: 12px; }.effort-option.selected { border-color: rgba(74, 213, 157, .55); color: var(--accent); background: rgba(74, 213, 157, .1); }
/* 先前输入区的 14px 上下内边距仍使默认框偏高，保留原规则以便追溯。 */
/* .conversation-page .composer-field .composer-input { display: block; height: auto; min-height: 22px; max-height: 154px; padding: 14px 54px 14px 17px; border-radius: 25px; line-height: 22px; } */
/* 会话输入区在本页的确定容器中成套定义：默认单行 42px，多行由 auto-height 向上扩展。 */
.conversation-page .composer-field .composer-input { display: block; height: auto; min-height: 22px; max-height: 154px; padding: 10px 50px 10px 16px; border-radius: 22px; line-height: 22px; }
/* 原强度按钮没有垂直对齐规则，原生按钮文字会贴到顶部。 */
.mobile-picker .effort-option { display: flex; height: 36px; min-height: 36px; padding: 0 12px; align-items: center; justify-content: center; line-height: 1; }
/* 首次覆盖只命中 markdown，未稳定压过消息组件样式；保留原规则以便追溯。 */
/* .conversation-page .markdown { padding: 10px 13px 12px; } */
/* 消息气泡内容保持上小下大：下内边距比上内边距多 2px。 */
.conversation-page .message .markdown { padding: 10px 13px 12px !important; }
/* 全局附件按钮的底部额外内边距会让加号偏上；仅在会话输入区取消该偏移。 */
.conversation-page .attachment-button { padding: 0; }
/* App 原生 textarea 在高度收紧后会露出默认白色底层；在会话输入容器内统一裁切和着色。 */
.conversation-page .composer-field { min-height: 42px; overflow: hidden; border-radius: 22px; background: var(--raised); }
.conversation-page .composer-field .composer-input { box-sizing: border-box; border: 0; outline: 0; background: var(--raised); }
.new-message-hint { position: fixed; z-index: 20; left: 50%; bottom: calc(84px + env(safe-area-inset-bottom)); display: flex; width: 38px; height: 38px; padding: 0; align-items: center; justify-content: center; transform: translateX(-50%); border: 1px solid var(--line); border-radius: 50%; color: var(--text); background: var(--raised); box-shadow: 0 6px 18px rgba(0, 0, 0, .28); font-size: 22px; line-height: 1; }
/* 历史图片是服务端返回的 data: 地址，在本页用全屏图层预览，避免原生 previewImage 不支持该地址。 */
.message-image-preview-backdrop { position: fixed; z-index: 2000; inset: 0; display: flex; padding: 28px 18px calc(28px + env(safe-area-inset-bottom)); align-items: center; justify-content: center; background: rgba(0, 0, 0, .88); }
.message-image-preview { width: 100%; height: 100%; }
.message-image-preview-close { position: absolute; top: calc(14px + env(safe-area-inset-top)); right: 14px; display: flex; width: 38px; height: 38px; padding: 0; align-items: center; justify-content: center; border-radius: 50%; color: #fff; background: rgba(255, 255, 255, .18); font-size: 28px; line-height: 1; }
</style>
