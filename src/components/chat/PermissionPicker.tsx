import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  Check,
  ChevronDown,
  Shield,
  Zap,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import type { PermissionComponentJson } from "../../types";

type UnifiedPermissionOption = {
  /** 统一权限参数的稳定值。 */
  value: string;
  /** 面向用户显示的权限选项名称。 */
  label: string;
  /** 当前选项的业务说明。 */
  description?: string;
};

const PERMISSION_OPTIONS: Record<string, UnifiedPermissionOption[]> = {
  /** 统一权限预设选项。 */
  autonomyPreset: [
    { value: "automatic", label: "自动" },
    { value: "read-only", label: "只读" },
    { value: "ask", label: "操作前询问" },
    { value: "auto", label: "工作区内自动" },
    { value: "full", label: "完全自主" },
  ],
  /** 工作区或项目的信任选项。 */
  trust: [
    { value: "automatic", label: "自动" },
    { value: "trusted", label: "受信任" },
    { value: "standard", label: "标准" },
    { value: "restricted", label: "受限" },
  ],
  /** 权限对象中的审批级别选项。 */
  approval: [
    { value: "automatic", label: "自动" },
    { value: "restricted", label: "只读" },
    { value: "ask", label: "操作前询问" },
    { value: "autonomous", label: "完全自主" },
  ],
  /** 权限对象中的沙箱级别选项。 */
  sandbox: [
    { value: "automatic", label: "自动" },
    { value: "read-only", label: "只读" },
    { value: "workspace-write", label: "工作区可写" },
    { value: "full-access", label: "完全访问" },
  ],
  /** 权限对象中的网络访问选项。 */
  network: [
    { value: "automatic", label: "自动" },
    { value: "enabled", label: "允许网络" },
    { value: "restricted", label: "限制网络" },
  ],
};

type PermissionComponentKey = keyof PermissionComponentJson;
const SECTION_IDS: Array<Exclude<PermissionComponentKey, "autonomyPreset" | "defaultForNewThreads">> = [
  "trust",
  "approval",
  "sandbox",
  "network",
];

/** 读取权限组件某个统一参数当前使用的首个选项。 */
function unifiedValue(values: PermissionComponentJson, key: PermissionComponentKey): string | null {
  const value = values[key];
  return Array.isArray(value) && typeof value[0] === "string" ? value[0] : null;
}

/** 更新统一权限参数；修改高级参数时清空预设，表示当前权限进入自定义状态。 */
export function updatePermissionComponentValue(
  values: PermissionComponentJson,
  key: PermissionComponentKey,
  next: string,
): PermissionComponentJson {
  // 更新被选中的统一权限属性，并在高级选项变化时将预设标为自定义。
  const updated = { ...values, [key]: [next] };
  if (key !== "trust" && key !== "autonomyPreset") {
    updated.autonomyPreset = [];
  }
  return updated;
}

/** 将空权限数据补齐为组件固定六键，供线程切换和契约测试复用。 */
export function normalizePermissionComponent(values: Partial<PermissionComponentJson>): PermissionComponentJson {
  return {
    autonomyPreset: Array.isArray(values.autonomyPreset) ? values.autonomyPreset : ["automatic"],
    trust: Array.isArray(values.trust) ? values.trust : ["automatic"],
    approval: Array.isArray(values.approval) ? values.approval : ["automatic"],
    sandbox: Array.isArray(values.sandbox) ? values.sandbox : ["automatic"],
    network: Array.isArray(values.network) ? values.network : ["automatic"],
    defaultForNewThreads: Array.isArray(values.defaultForNewThreads) ? values.defaultForNewThreads : [],
  };
}

/** 只有当前预设与默认预设完全相同，默认用于新会话复选框才显示选中。 */
export function isPermissionDefaultSelected(
  preset: string | null,
  defaultValues: string[],
): boolean {
  return preset !== null && defaultValues[0] === preset;
}

export interface PermissionPickerProps {
  /** 组件交互禁用状态。 */
  disabled?: boolean;
  /** 当前完整统一权限 JSON。 */
  value: PermissionComponentJson;
  /** 修改后返回完整统一权限 JSON。 */
  onChange: (value: PermissionComponentJson) => void;
}

/** 固定权限交互组件；不识别任何 CLI 原始权限或 CLI 标识。 */
export function PermissionPicker({ disabled = false, value, onChange }: PermissionPickerProps) {
  const [open, setOpen] = useState(false);
  const [activeSection, setActiveSection] = useState<PermissionComponentKey>("autonomyPreset");
  const triggerRef = useRef<HTMLButtonElement>(null);
  const popoverRef = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ bottom: 0, left: 0 });
  const preset = unifiedValue(value, "autonomyPreset");
  const title = preset
    ? PERMISSION_OPTIONS.autonomyPreset.find((item) => item.value === preset)?.label ?? "自动"
    : "自定义";

  useLayoutEffect(() => {
    if (!open || !triggerRef.current) return;
    const rect = triggerRef.current.getBoundingClientRect();
    setPos({ bottom: window.innerHeight - rect.top + 6, left: Math.max(8, Math.min(rect.left, window.innerWidth - 460)) });
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as HTMLElement;
      if (!triggerRef.current?.contains(target) && !popoverRef.current?.contains(target)) setOpen(false);
    };
    const onKeyDown = (event: KeyboardEvent) => { if (event.key === "Escape") setOpen(false); };
    document.addEventListener("pointerdown", onPointerDown, true);
    document.addEventListener("keydown", onKeyDown, true);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown, true);
      document.removeEventListener("keydown", onKeyDown, true);
    };
  }, [open]);

  const select = useCallback((key: PermissionComponentKey, next: string) => {
    const updated = updatePermissionComponentValue(value, key, next);
    onChange(updated);
  }, [onChange, value]);

  const panelOptions = PERMISSION_OPTIONS[activeSection] ?? [];
  const panelTitle = activeSection === "autonomyPreset"
    ? "权限模式"
    : ({ trust: "信任", approval: "审批", sandbox: "沙箱", network: "网络" } as Record<string, string>)[activeSection];
  const sections: PermissionComponentKey[] = ["autonomyPreset", ...SECTION_IDS];
  const defaultSelected = isPermissionDefaultSelected(preset, value.defaultForNewThreads);

  const popover = open ? createPortal(
    <div ref={popoverRef} className="pp-popover" style={{ position: "fixed", bottom: pos.bottom, left: pos.left }}>
      <div className="pp-rail">
        <div className="pp-rail-label">权限</div>
        {sections.map((section) => (
          <button key={section} type="button" className={`pp-rail-item${activeSection === section ? " pp-rail-item-active" : ""}`} onClick={() => setActiveSection(section)}>
            <span className="pp-rail-item-icon">{section === "autonomyPreset" ? <Zap size={13} /> : <Shield size={13} />}</span>
            <span className="pp-rail-item-name">{section === "autonomyPreset" ? "权限模式" : panelTitle && section === activeSection ? panelTitle : ({ trust: "信任", approval: "审批", sandbox: "沙箱", network: "网络" } as Record<string, string>)[section]}</span>
          </button>
        ))}
      </div>
      <div className="pp-panel">
        <div className="pp-panel-header"><div className="pp-panel-title"><span>{panelTitle}</span></div></div>
        <div className="pp-panel-content" id={activeSection}><div className="pp-options">
          {panelOptions.map((option) => {
            const selected = unifiedValue(value, activeSection) === option.value;
            return <button key={option.value} type="button" className={`pp-option${selected ? " pp-option-selected" : ""}`} disabled={disabled} data-permission-value={option.value} onClick={() => select(activeSection, option.value)}><div className="pp-option-copy"><span className="pp-option-label">{option.label}</span>{option.description ? <span className="pp-option-description">{option.description}</span> : null}</div>{selected ? <Check size={13} className="pp-option-check" /> : null}</button>;
          })}
        </div></div>
        {activeSection === "autonomyPreset" ? <label className={`pp-default-toggle${preset === null ? " pp-default-toggle-disabled" : ""}`}><input id="defaultForNewThreads" type="checkbox" checked={defaultSelected} disabled={disabled || preset === null} onChange={(event) => { if (preset !== null) onChange({ ...value, defaultForNewThreads: event.target.checked ? [preset] : [] }); }} />默认用于新会话</label> : null}
      </div>
    </div>, document.body,
  ) : null;

  return <div className="pp-root"><button ref={triggerRef} type="button" className={`pp-trigger${open ? " pp-trigger-open" : ""}`} onClick={() => !disabled && setOpen((current) => !current)} disabled={disabled} title={title}><span className="pp-trigger-icon"><Zap size={12} /></span><span className="pp-trigger-label">{title}</span><ChevronDown size={10} className={`pp-trigger-chevron${open ? " pp-trigger-chevron-open" : ""}`} /></button>{popover}</div>;
}
