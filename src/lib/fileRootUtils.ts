/** 规范化跨平台绝对路径，统一分隔符和盘符大小写。 */
export function normalizeAbsolutePath(path: string): string {
  let normalized = path.replace(/\\/g, "/");
  if (/^\/[A-Za-z]:\//.test(normalized)) normalized = normalized.slice(1);
  if (/^[A-Za-z]:\//.test(normalized)) normalized = `${normalized[0].toUpperCase()}${normalized.slice(1)}`;
  const isUnc = normalized.startsWith("//");
  normalized = normalized.replace(/\/+/g, "/");
  if (isUnc) normalized = `//${normalized.replace(/^\/+/, "")}`;
  if (normalized.length > 1 && /\/$/.test(normalized) && !/^[A-Za-z]:\/$/.test(normalized)) normalized = normalized.replace(/\/+$/, "");
  return normalized;
}

/** 判断文件绝对路径是否位于项目根目录内。 */
export function isWithinRoot(absolutePath: string, rootPath: string): boolean {
  const normalizedPath = normalizeAbsolutePath(absolutePath);
  const normalizedRoot = normalizeAbsolutePath(rootPath);
  const comparisonPath = /^[A-Za-z]:\//.test(normalizedPath) || normalizedPath.startsWith("//") ? normalizedPath.toLowerCase() : normalizedPath;
  const comparisonRoot = /^[A-Za-z]:\//.test(normalizedRoot) || normalizedRoot.startsWith("//") ? normalizedRoot.toLowerCase() : normalizedRoot;
  return comparisonPath === comparisonRoot || comparisonPath.startsWith(`${comparisonRoot}/`);
}

/** 将项目内文件路径解析为绝对路径。 */
export function resolveAbsoluteFilePath(rootPath: string, filePath: string): string {
  const normalizedRoot = normalizeAbsolutePath(rootPath);
  const normalizedFilePath = filePath.replace(/\\/g, "/").replace(/^\/+/, "");
  return normalizeAbsolutePath(normalizedFilePath ? `${normalizedRoot}/${normalizedFilePath}` : normalizedRoot);
}

/** 将项目内绝对路径转换为相对项目根的文件路径。 */
export function resolveRelativePathWithinRoot(absolutePath: string, rootPath: string): string | null {
  if (!isWithinRoot(absolutePath, rootPath)) return null;
  const normalizedAbsolutePath = normalizeAbsolutePath(absolutePath);
  const normalizedRootPath = normalizeAbsolutePath(rootPath);
  if (normalizedAbsolutePath.toLowerCase() === normalizedRootPath.toLowerCase()) return "";
  return normalizedAbsolutePath.slice(normalizedRootPath.length).replace(/^\/+/, "");
}
