import type { AttachmentBatchItem, ChatAttachment } from "./types";
import { auracoderDeviceStore } from "./stores/auracoder-device";

const MAX_ATTACHMENT_BYTES = 10 * 1024 * 1024;
/** HTTP 附件服务连接参数；不含文件内容，只保存鉴权与路由信息。 */
interface UploadContext {
  /** HTTP 上传服务根地址。 */
  uploadRoot: string;
  /** 当前已绑定设备的稳定 ID。 */
  deviceId: string;
  /** Relay 鉴权凭据。 */
  relayCredential: string;
  /** 当前 AuraCoder 的 Tunnel ID。 */
  tunnelId: string;
}

/** HTTP 201 返回的附件元数据。 */
interface HttpAttachmentResponse {
  /** 服务端附件引用键。 */
  attachment_key: string;
  /** 服务端确认的文件名。 */
  file_name: string;
  /** 服务端确认的文件字节数。 */
  size_bytes: number;
  /** 服务端确认的 MIME 类型。 */
  mime_type: string;
  /** 服务端确认的设备 ID。 */
  device_id: string;
  /** 服务端确认的批次 ID。 */
  batch_id: string;
}

function errorMessage(value: unknown, fallback: string) {
  if (value instanceof Error && value.message) return value.message;
  if (value && typeof value === "object") {
    const record = value as Record<string, unknown>;
    if (typeof record.errMsg === "string" && record.errMsg) return record.errMsg;
    if (typeof record.message === "string" && record.message) return record.message;
    if (typeof record.error === "string" && record.error) return record.error;
  }
  return fallback;
}

function resolveUploadRoot(endpoint: string) {
  try {
    const normalizedEndpoint = endpoint.replace(/^wss:/i, "https:").replace(/^ws:/i, "http:");
    const parsed = new URL(normalizedEndpoint);
    parsed.pathname = "";
    parsed.search = "";
    parsed.hash = "";
    return parsed.toString().replace(/\/$/, "");
  } catch {
    const normalizedEndpoint = endpoint.replace(/^wss:/i, "https:").replace(/^ws:/i, "http:");
    const authority = /^([a-z][a-z\d+.-]*:\/\/[^/]+)/i.exec(normalizedEndpoint)?.[1];
    return authority || normalizedEndpoint.replace(/\/$/, "");
  }
}

function getUploadContext(auracoderId: string): UploadContext {
  const device = auracoderDeviceStore.getDevice(auracoderId);
  if (!device) throw new Error("AuraCoder 设备不存在");
  if (!device.deviceId) throw new Error("设备未完成绑定");
  return {
    // endpoint 只保留协议、主机和端口，HTTP 路径由固定接口拼接。
    uploadRoot: resolveUploadRoot(device.endpoint),
    // HTTP 与 WSS 共用当前设备 ID。
    deviceId: device.deviceId,
    // 上传接口要求 Relay 凭据请求头。
    relayCredential: device.relayCredential,
    // 表单中用于隔离 Tunnel 的 ID。
    tunnelId: device.tunnelId,
  };
}

function compressImagePath(filePath: string): Promise<string> {
  return new Promise((resolve, reject) => {
    uni.compressImage({
      // 图片选择器返回的原始路径。
      src: filePath,
      // 按协议要求使用 80 质量压缩。
      quality: 80,
      // 压缩后仍由 UniApp 生成可供 uploadFile 使用的本地路径。
      success: (result) => resolve(String(result.tempFilePath || filePath)),
      // 保留原生 errMsg/message，便于定位真机路径问题。
      fail: (error) => reject(new Error(`图片压缩失败：${errorMessage(error, "未知错误")}`)),
    });
  });
}

function uploadFile(
  context: UploadContext,
  batchId: string,
  attachment: AttachmentBatchItem,
  filePath: string,
): Promise<HttpAttachmentResponse> {
  return new Promise((resolve, reject) => {
    uni.uploadFile({
      // 固定 HTTP 附件上传接口。
      url: `${context.uploadRoot}/api/mobile/attachments`,
      // 压缩后的图片路径或普通附件原始路径。
      filePath,
      // multipart 文件字段名固定为 file。
      name: "file",
      // Relay 凭据只放请求头，不写入 URL 或文件名。
      header: { "x-auracoder-relay-credential": context.relayCredential },
      // 服务端用于校验设备、批次与文件来源的表单字段。
      formData: {
        // Tunnel 路由标识。
        tunnel_id: context.tunnelId,
        // 当前绑定设备标识。
        device_id: context.deviceId,
        // 一次点击发送形成的批次标识。
        batch_id: batchId,
        // 原始选择文件名。
        file_name: attachment.fileName,
        // 文件 MIME 类型。
        mime_type: attachment.mimeType || "application/octet-stream",
        // 图片或普通附件来源。
        attachment_kind: attachment.source,
      },
      // 只接受 201，并解析服务端返回的附件键。
      success: (result) => {
        if (Number(result.statusCode) !== 201) {
          reject(new Error(`附件上传失败（HTTP ${result.statusCode}）`));
          return;
        }
        try {
          const payload = typeof result.data === "string" ? JSON.parse(result.data) as HttpAttachmentResponse : result.data as unknown as HttpAttachmentResponse;
          if (!payload || typeof payload.attachment_key !== "string" || !payload.attachment_key) {
            reject(new Error("附件上传响应缺少 attachment_key"));
            return;
          }
          resolve(payload);
        } catch (error) {
          reject(new Error(`附件上传响应解析失败：${errorMessage(error, "JSON 无效")}`));
        }
      },
      // 不记录响应正文，避免日志暴露文件内容。
      fail: (error) => reject(new Error(`附件上传请求失败：${errorMessage(error, "网络错误")}`)),
    });
  });
}
// 旧版 WSS 分块上传大小保留作协议迁移追溯；HTTP multipart 上传不再使用。
// const UPLOAD_CHUNK_BASE64_SIZE = 256 * 1024;
// 旧版批次并发常量保留作迁移追溯；跨端 uploadFile 严格逐个执行。
// const MAX_UPLOAD_CONCURRENCY = 2;

/** 选择后暂存在手机端的文件数据；dataUrl 只在发送批次上传期间生成。 */
export interface LocalFileData {
  /** 文件完整 data URL，上传时去掉头部后分块发送。 */
  dataUrl: string;
  /** 供协议使用的文件名。 */
  fileName: string;
  /** 文件字节数。 */
  sizeBytes: number;
  /** 文件 MIME 类型。 */
  mimeType: string;
}

/** 选择后只读取元数据，不读取整个文件内容。 */
export interface LocalFileMetadata {
  /** 文件名。 */
  fileName: string;
  /** 文件字节数；无法取得时为 0。 */
  sizeBytes: number;
  /** 文件 MIME 类型。 */
  mimeType: string;
}

function fileNameFromPath(path: string, fallbackName: string) {
  const value = path.split(/[\\/]/).pop() || fallbackName;
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}

function isAndroidContentUri(filePath: string) {
  return /^content:\/\//i.test(filePath);
}

/*
// 通过 Android ContentResolver 读取 content:// 的旧实现。
function readWithAndroid(filePath: string, fallbackName: string, fallbackMimeType: string): Promise<LocalFileData> {
  return new Promise((resolve, reject) => {
    try {
      if (typeof plus === "undefined" || !plus.android) throw new Error("当前设备无法读取所选文件");
      const activity = plus.android.runtimeMainActivity();
      const Uri = plus.android.importClass("android.net.Uri");
      const File = plus.android.importClass(["ja", "va.io.File"].join(""));
      const Base64 = plus.android.importClass("android.util.Base64");
      const Byte = plus.android.importClass(["ja", "va.lang.Byte"].join(""));
      const ReflectArray = plus.android.importClass(["ja", "va.lang.reflect.Array"].join(""));
      const URLConnection = plus.android.importClass(["ja", "va.net.URLConnection"].join(""));
      const resolver = plus.android.invoke(activity, "getContentResolver");
      let normalizedUri = filePath;
      if (normalizedUri.startsWith("_")) normalizedUri = plus.io.convertLocalFileSystemURL(normalizedUri);
      const contentUri = isAndroidContentUri(normalizedUri);
      const uri = normalizedUri.startsWith("/")
        ? Uri.fromFile(new File(normalizedUri))
        : Uri.parse(normalizedUri);
      let fileName = fallbackName;
      let sizeBytes = 0;
      let mimeType = fallbackMimeType;
      let input: any = null;
      if (contentUri) {
        let cursor: any = null;
        try {
          cursor = plus.android.invoke(resolver, "query", uri, null, null, null, null);
          if (cursor && plus.android.invoke(cursor, "moveToFirst")) {
            const nameIndex = Number(plus.android.invoke(cursor, "getColumnIndex", "_display_name"));
            const sizeIndex = Number(plus.android.invoke(cursor, "getColumnIndex", "_size"));
            if (nameIndex >= 0) fileName = String(plus.android.invoke(cursor, "getString", nameIndex) || fallbackName);
            if (sizeIndex >= 0) sizeBytes = Number(plus.android.invoke(cursor, "getLong", sizeIndex)) || 0;
          }
        } finally {
          if (cursor) plus.android.invoke(cursor, "close");
        }
        mimeType = String(plus.android.invoke(resolver, "getType", uri) || fallbackMimeType);
        input = plus.android.invoke(resolver, "openInputStream", uri);
      } else {
        const localPath = normalizedUri.startsWith("file://")
          ? String(plus.android.invoke(uri, "getPath") || normalizedUri.slice(7))
          : normalizedUri;
        const localFile = new File(localPath);
        fileName = String(plus.android.invoke(localFile, "getName") || fallbackName);
        sizeBytes = Number(plus.android.invoke(localFile, "length")) || 0;
        mimeType = String(plus.android.invoke(URLConnection, "guessContentTypeFromName", fileName) || fallbackMimeType);
        input = plus.android.newObject(["ja", "va.io.FileInputStream"].join(""), localFile);
      }
      if (sizeBytes > MAX_ATTACHMENT_BYTES) throw new Error("单个附件不能超过 10 MB");
      if (!input) throw new Error("无法打开所选文件");
      const output = plus.android.newObject(["ja", "va.io.ByteArrayOutputStream"].join(""));
      const byteType = plus.android.getAttribute(Byte, "TYPE");
      const buffer = plus.android.invoke(ReflectArray, "newInstance", byteType, 32768);
      try {
        let bytesRead = Number(plus.android.invoke(input, "read", buffer));
        while (bytesRead > 0) {
          plus.android.invoke(output, "write", buffer, 0, bytesRead);
          if (Number(plus.android.invoke(output, "size")) > MAX_ATTACHMENT_BYTES) throw new Error("单个附件不能超过 10 MB");
          bytesRead = Number(plus.android.invoke(input, "read", buffer));
        }
        sizeBytes = Number(plus.android.invoke(output, "size")) || sizeBytes;
        const byteArray = plus.android.invoke(output, "toByteArray");
        const base64 = String(plus.android.invoke(Base64, "encodeToString", byteArray, 2));
        resolve({ dataUrl: `data:${mimeType};base64,${base64}`, fileName, sizeBytes, mimeType });
      } finally {
        plus.android.invoke(input, "close");
        plus.android.invoke(output, "close");
      }
    } catch (error) {
      console.error("[附件] Android 文件读取失败", error);
      reject(error instanceof Error ? error : new Error(`读取所选文件失败：${String(error)}`));
    }
  });
}
*/

/** 上传单个附件；图片先压缩，普通附件直接使用选择器路径。 */
export async function uploadAttachmentInBatch(
  auracoderId: string,
  batchId: string,
  attachment: AttachmentBatchItem,
  cancelled: () => boolean = () => false,
): Promise<ChatAttachment> {
  if (attachment.attachmentKey && !attachment.failed) return { ...attachment, uploading: false };
  if (!attachment.localPath) throw new Error(`附件 ${attachment.fileName} 缺少本地路径`);
  if (cancelled()) throw new Error("上传已取消");
  const context = getUploadContext(auracoderId);
  const filePath = attachment.source === "image"
    ? await compressImagePath(attachment.localPath)
    : attachment.localPath;
  if (cancelled()) throw new Error("上传已取消");
  const payload = await uploadFile(context, batchId, attachment, filePath);
  return {
    // 保留编辑区稳定 ID。
    id: attachment.id,
    // 使用服务端确认的文件名。
    fileName: payload.file_name || attachment.fileName,
    // 新协议不再使用旧 filePath 引用。
    filePath: "",
    // message.send 使用的服务端附件键。
    attachmentKey: payload.attachment_key,
    // 选择阶段保留的本地路径仅供重试。
    localPath: attachment.localPath,
    // 选择入口来源。
    source: attachment.source,
    // 使用服务端确认的大小。
    sizeBytes: Number(payload.size_bytes) || attachment.sizeBytes,
    // 使用服务端确认的 MIME。
    mimeType: payload.mime_type || attachment.mimeType,
    // 单文件上传已完成。
    uploading: false,
    // 清除旧失败标记。
    failed: false,
    // 清除旧错误。
    error: undefined,
  };
}

/** 一个批次内严格逐个调用 uploadFile，避免真机图片读取并发冲突。 */
export async function uploadAttachmentBatch(
  auracoderId: string,
  batchId: string,
  attachments: AttachmentBatchItem[],
  cancelled: () => boolean = () => false,
): Promise<void> {
  for (const item of attachments) {
    if (cancelled()) throw new Error("上传已取消");
    if (item.attachmentKey && !item.failed) continue;
    item.uploading = true;
    item.failed = false;
    item.error = undefined;
    try {
      const uploaded = await uploadAttachmentInBatch(auracoderId, batchId, item, cancelled);
      Object.assign(item, uploaded, { uploading: false, failed: false, error: undefined });
    } catch (error) {
      item.uploading = false;
      item.failed = true;
      item.error = errorMessage(error, "附件上传失败");
      throw error;
    }
  }
}

function deleteUploadedAttachment(context: UploadContext, attachmentKey: string): Promise<void> {
  return new Promise((resolve, reject) => {
    uni.request({
      // 固定 HTTP 删除接口。
      url: `${context.uploadRoot}/api/mobile/attachments/${encodeURIComponent(attachmentKey)}?tunnel_id=${encodeURIComponent(context.tunnelId)}`,
      // 仅删除当前批次已经获得的附件键。
      method: "DELETE",
      // 删除与上传使用同一个 Relay 凭据头。
      header: { "x-auracoder-relay-credential": context.relayCredential },
      success: (result) => {
        if (result.statusCode >= 200 && result.statusCode < 300) resolve();
        else reject(new Error(`附件清理失败（HTTP ${result.statusCode}）`));
      },
      fail: (error) => reject(new Error(`附件清理请求失败：${errorMessage(error, "网络错误")}`)),
    });
  });
}

/** 取消批次时逐个删除已经获得的附件键；不发送 WSS abort。 */
export async function deleteBatchAttachments(auracoderId: string, attachments: AttachmentBatchItem[]) {
  const keys = attachments.map((item) => item.attachmentKey).filter((key): key is string => Boolean(key));
  if (!keys.length) return;
  const context = getUploadContext(auracoderId);
  let firstError: unknown;
  for (const attachmentKey of keys) {
    try {
      await deleteUploadedAttachment(context, attachmentKey);
    } catch (error) {
      // 即使一个删除失败，也继续为同一批次的其它附件发出 DELETE。
      firstError ||= error;
    }
  }
  if (firstError) throw firstError;
}

/* 旧版 App-Plus FileReader 路径读取保留，不再执行；HTTP uploadFile 直接接收选择器路径。 */
/*
function readWithPlus(filePath: string, fallbackName: string, fallbackMimeType: string): Promise<LocalFileData> {
  if (isAndroidContentUri(filePath)) return readWithAndroid(filePath, fallbackName, fallbackMimeType);
  return new Promise((resolve, reject) => {
    plus.io.resolveLocalFileSystemURL(filePath, (entry: any) => {
      entry.file((file: any) => {
        if (Number(file.size) > MAX_ATTACHMENT_BYTES) {
          reject(new Error("单个附件不能超过 10 MB"));
          return;
        }
        const reader = new plus.io.FileReader();
        reader.onloadend = () => {
          const dataUrl = String(reader.result || "");
          if (!dataUrl.startsWith("data:") || !dataUrl.includes(";base64,")) {
            reject(new Error("附件数据格式无效"));
            return;
          }
          resolve({
            dataUrl,
            fileName: String(file.name || entry.name || fallbackName),
            sizeBytes: Number(file.size) || 0,
            mimeType: String(file.type || fallbackMimeType),
          });
        };
        reader.onerror = () => reject(new Error("读取附件失败"));
        reader.readAsDataURL(file);
      }, () => reject(new Error("读取附件失败")));
    }, () => reject(new Error("无法访问所选附件")));
  });
}
*/

/* 旧版 H5 fetch/FileReader 读取保留，不再执行。 */
/*
async function readWithFetch(filePath: string, fallbackName: string, fallbackMimeType: string): Promise<LocalFileData> {
  const response = await fetch(filePath);
  if (!response.ok) throw new Error("读取附件失败");
  const blob = await response.blob();
  if (blob.size > MAX_ATTACHMENT_BYTES) throw new Error("单个附件不能超过 10 MB");
  const dataUrl = await new Promise<string>((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result || ""));
    reader.onerror = () => reject(new Error("读取附件失败"));
    reader.readAsDataURL(blob);
  });
  return {
    dataUrl,
    fileName: fileNameFromPath(filePath, fallbackName),
    sizeBytes: blob.size,
    mimeType: blob.type || fallbackMimeType,
  };
}
*/

/** 选择阶段只通过 UniApp 标准 API 获取文件大小，不读取文件内容。 */
export async function inspectLocalFile(filePath: string, fallbackName: string, fallbackMimeType: string): Promise<LocalFileMetadata> {
  const fallback: LocalFileMetadata = {
    // 路径解析得到的回退文件名。
    fileName: fileNameFromPath(filePath, fallbackName),
    // 无法读取大小时由服务端校验。
    sizeBytes: 0,
    // 选择器提供的回退 MIME。
    mimeType: fallbackMimeType,
  };
  if (!filePath || typeof uni.getFileInfo !== "function") return fallback;
  return new Promise<LocalFileMetadata>((resolve, reject) => {
    uni.getFileInfo({
      // UniApp 标准本地文件路径或 content URI。
      filePath,
      // 成功回调只返回大小，文件名和 MIME 仍使用选择器信息。
      success: (result) => {
        const sizeBytes = Number(result.size) || 0;
        if (sizeBytes > MAX_ATTACHMENT_BYTES) {
          reject(new Error("单个附件不能超过 10 MB"));
          return;
        }
        resolve({ ...fallback, sizeBytes });
      },
      // 元数据失败不阻止选择，实际 uploadFile 失败时保留底层错误。
      fail: () => resolve(fallback),
    });
  });
}

/* 旧 WSS attachment.upload 分块实现保留作迁移追溯，HTTP multipart 实现见下方。 */
/*
function createUploadId(batchId: string, attachmentId: string) {
  return `${batchId}-${attachmentId}-${Date.now().toString(36)}-${Math.random().toString(16).slice(2)}`;
}

// 上传单个批次附件；旧版所有分块都带同一 batch_id。
export async function uploadAttachmentInBatch(
  auracoderId: string,
  batchId: string,
  attachment: AttachmentBatchItem,
  cancelled: () => boolean = () => false,
): Promise<ChatAttachment> {
  if (attachment.filePath && !attachment.failed) return { ...attachment, uploading: false };
  if (!attachment.localPath) throw new Error(`附件 ${attachment.fileName} 缺少本地路径`);
  const localFile = await readLocalFile(attachment.localPath, attachment.fileName, attachment.mimeType || "application/octet-stream");
  const prefix = /^data:([^;,]+);base64,/i.exec(localFile.dataUrl);
  const commaIndex = localFile.dataUrl.indexOf(",");
  if (!prefix || commaIndex < 0) throw new Error("附件数据格式无效");
  const dataBase64 = localFile.dataUrl.slice(commaIndex + 1);
  const chunkCount = Math.max(1, Math.ceil(dataBase64.length / UPLOAD_CHUNK_BASE64_SIZE));
  if (chunkCount > 64) throw new Error("单个附件不能超过 10 MB");
  const uploadId = createUploadId(batchId, attachment.id);
  let completed: ChatAttachment | null = null;
  for (let chunkIndex = 0; chunkIndex < chunkCount; chunkIndex += 1) {
    if (cancelled()) throw new Error("上传已取消");
    completed = await auracoderConnectionManager.request<ChatAttachment>(auracoderId, "attachment.upload", {
      // 同一发送批次的 UUID。
      batch_id: batchId,
      // 同一文件所有分块共用的上传标识。
      upload_id: uploadId,
      // 文件名。
      file_name: localFile.fileName,
      // 文件 MIME 类型。
      mime_type: (localFile.mimeType || prefix[1]).toLowerCase(),
      // 当前分块序号。
      chunk_index: chunkIndex,
      // 文件总分块数。
      chunk_count: chunkCount,
      // 当前分块的 Base64 内容。
      data_base64: dataBase64.slice(chunkIndex * UPLOAD_CHUNK_BASE64_SIZE, (chunkIndex + 1) * UPLOAD_CHUNK_BASE64_SIZE),
    });
  }
  if (cancelled()) throw new Error("上传已取消");
  if (!completed?.filePath) throw new Error("附件上传没有返回文件路径");
  return {
    ...completed,
    id: attachment.id,
    fileName: localFile.fileName,
    sizeBytes: localFile.sizeBytes || completed.sizeBytes,
    mimeType: localFile.mimeType || completed.mimeType,
    uploading: false,
    failed: false,
    error: undefined,
    source: attachment.source,
    localPath: attachment.localPath,
  };
}

// 旧版一个批次最多并发上传两个文件。
export async function uploadAttachmentBatch(
  auracoderId: string,
  batchId: string,
  attachments: AttachmentBatchItem[],
  cancelled: () => boolean = () => false,
): Promise<void> {
  let nextIndex = 0;
  let firstError: unknown;
  const worker = async () => {
    while (!firstError && !cancelled()) {
      const index = nextIndex;
      nextIndex += 1;
      const item = attachments[index];
      if (!item) return;
      if (item.filePath && !item.failed) continue;
      item.uploading = true;
      item.failed = false;
      item.error = undefined;
      try {
        const uploaded = await uploadAttachmentInBatch(auracoderId, batchId, item, cancelled);
        Object.assign(item, uploaded, { uploading: false, failed: false, error: undefined });
      } catch (error) {
        item.uploading = false;
        item.failed = true;
        item.error = error instanceof Error ? error.message : String(error);
        firstError = error;
      }
    }
  };
  const workerCount = Math.min(MAX_UPLOAD_CONCURRENCY, Math.max(1, attachments.length));
  await Promise.all(Array.from({ length: workerCount }, () => worker()));
  if (cancelled() && !firstError) throw new Error("上传已取消");
  if (firstError) throw firstError;
}

// 兼容旧调用点的单文件上传入口。
export async function readAndUploadAttachment(
  auracoderId: string,
  filePath: string,
  fallbackName: string,
  fallbackMimeType: string,
  cancelled: () => boolean,
) {
  const attachment: AttachmentBatchItem = {
    id: `mobile-${Date.now()}-${Math.random().toString(16).slice(2)}`,
    fileName: fallbackName,
    filePath: "",
    localPath: filePath,
    sizeBytes: 0,
    mimeType: fallbackMimeType,
    source: fallbackMimeType.startsWith("image/") ? "image" : "file",
  };
  return uploadAttachmentInBatch(auracoderId, `mobile-legacy-${Date.now()}`, attachment, cancelled);
}
*/
