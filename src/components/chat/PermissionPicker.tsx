import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import {
  Check,
  ChevronDown,
  ChevronLeft,
  Eye,
  FolderOpen,
  Monitor,
  Shield,
  SquareTerminal,
  Zap,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import type { PermissionComponentJson } from "../../types";

/*
 * 旧版按 CLI 分支渲染的实现保留在本文件末尾注释中，作为迁移记录。
 */

/*

type PermissionOption<T extends string = string> = {
  value: T;
  label: string;
  description?: string;
};

type RailItem = {
  id: string;
  icon: ReactNode;
  title: string;
  currentLabel: string | null;
  options: PermissionOption[];
  value: string;
  onChange?: (value: string) => void;
  disabled?: boolean;
  note?: string | null;
};

interface PermissionPickerProps {
  disabled?: boolean;
  trustScopeLabel?: string;
  trustValue?: TrustLevel;
  trustOptions?: PermissionOption<TrustLevel>[];
  onTrustChange?: (value: TrustLevel) => void;
  customPolicyCount?: number;
  engineId?: ChatEngineId;
  presetValue?: AutonomyPresetId | null;
  codexExternalSandbox?: boolean;
  onPresetChange?: (preset: AutonomyPresetId) => void;
  defaultPreset?: AutonomyPresetId | null;
  onDefaultPresetChange?: (preset: AutonomyPresetId | null) => void;
  approvalTitle?: string;
  approvalValue?: string;
  approvalSelectedLabel?: string | null;
  approvalOptions?: PermissionOption[];
  onApprovalChange?: (value: string) => void;
  sandboxValue?: string;
  sandboxOptions?: PermissionOption[];
  onSandboxChange?: (value: string) => void;
  sandboxNotice?: string | null;
  sandboxSelectedLabel?: string | null;
  networkValue?: string;
  networkOptions?: PermissionOption[];
  onNetworkChange?: (value: string) => void;
  networkDisabled?: boolean;
  networkNotice?: string | null;
}

const PRESET_ICONS: Record<AutonomyPresetId, ReactNode> = {
  inherit: <Shield size={13} />,
  "read-only": <Eye size={13} />,
  ask: <Shield size={13} />,
  auto: <FolderOpen size={13} />,
  full: <Zap size={13} />,
};

const PRESET_TRIGGER_ICONS: Record<AutonomyPresetId, ReactNode> = {
  inherit: <Shield size={12} />,
  "read-only": <Eye size={12} />,
  ask: <Shield size={12} />,
  auto: <FolderOpen size={12} />,
  full: <Zap size={12} />,
};

function findOption<T extends string>(
  options: PermissionOption<T>[] | undefined,
  value: T | string | undefined,
): PermissionOption<T> | null {
  if (!options || !value) {
    return null;
  }
  return options.find((option) => option.value === value) ?? null;
}

export function PermissionPicker({
  disabled = false,
  trustScopeLabel,
  trustValue,
  trustOptions,
  onTrustChange,
  customPolicyCount = 0,
  engineId,
  presetValue,
  codexExternalSandbox = false,
  onPresetChange,
  defaultPreset,
  onDefaultPresetChange,
  approvalTitle,
  approvalValue,
  approvalSelectedLabel,
  approvalOptions,
  onApprovalChange,
  sandboxValue,
  sandboxOptions,
  onSandboxChange,
  sandboxNotice,
  sandboxSelectedLabel,
  networkValue,
  networkOptions,
  onNetworkChange,
  networkDisabled = false,
  networkNotice,
}: PermissionPickerProps) {
  const { t } = useTranslation("chat");
  const [open, setOpen] = useState(false);
  const [activeSection, setActiveSection] = useState<string>("");
  const presetsAvailable =
    engineId !== undefined && presetValue !== undefined && onPresetChange !== undefined;
  const [view, setView] = useState<"presets" | "advanced">(
    presetsAvailable ? "presets" : "advanced",
  );
  const triggerRef = useRef<HTMLButtonElement>(null);
  const popoverRef = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ bottom: 0, left: 0 });

  const resolvedApprovalTitle = approvalTitle ?? t("permissionPicker.approvalPolicy");

  const trustOption = useMemo(
    () => findOption(trustOptions, trustValue),
    [trustOptions, trustValue],
  );

  const railItems = useMemo<RailItem[]>(() => {
    const items: RailItem[] = [];

    if (trustScopeLabel && trustValue && trustOptions && onTrustChange) {
      items.push({
        id: "trust",
        icon: <Shield size={13} />,
        title: trustScopeLabel,
        currentLabel: findOption(trustOptions, trustValue)?.label ?? null,
        options: trustOptions as PermissionOption[],
        value: trustValue,
        onChange: (value) => onTrustChange(value as TrustLevel),
      });
    }

    if (approvalOptions && approvalValue !== undefined) {
      items.push({
        id: "approval",
        icon: <Shield size={13} />,
        title: resolvedApprovalTitle,
        currentLabel:
          approvalSelectedLabel ??
          findOption(approvalOptions, approvalValue)?.label ??
          null,
        options: approvalOptions,
        value: approvalValue,
        onChange: onApprovalChange,
      });
    }

    if (sandboxOptions && sandboxValue !== undefined) {
      items.push({
        id: "sandbox",
        icon: <SquareTerminal size={13} />,
        title: t("permissionPicker.sandboxMode"),
        currentLabel:
          sandboxSelectedLabel ??
          findOption(sandboxOptions, sandboxValue)?.label ??
          null,
        options: sandboxOptions,
        value: sandboxValue,
        onChange: onSandboxChange,
        note: sandboxNotice,
      });
    }

    if (networkOptions && networkValue !== undefined) {
      items.push({
        id: "network",
        icon: <Monitor size={13} />,
        title: t("permissionPicker.networkAccess"),
        currentLabel: findOption(networkOptions, networkValue)?.label ?? null,
        options: networkOptions,
        value: networkValue,
        onChange: onNetworkChange,
        disabled: networkDisabled,
        note: networkNotice,
      });
    }

    return items;
  }, [
    networkDisabled,
    networkNotice,
    networkOptions,
    networkValue,
    onApprovalChange,
    onNetworkChange,
    onSandboxChange,
    onTrustChange,
    approvalOptions,
    approvalValue,
    resolvedApprovalTitle,
    sandboxNotice,
    sandboxOptions,
    sandboxSelectedLabel,
    sandboxValue,
    t,
    trustOptions,
    trustScopeLabel,
    trustValue,
  ]);

  useEffect(() => {
    if (open) {
      if (railItems.length > 0 && !activeSection) {
        setActiveSection(railItems[0].id);
      }
    } else {
      setActiveSection("");
      setView(presetsAvailable ? "presets" : "advanced");
    }
  }, [activeSection, open, presetsAvailable, railItems]);

  const summaryLines = useMemo(() => {
    const lines: string[] = [];
    if (trustScopeLabel && trustOption) {
      lines.push(`${trustScopeLabel}: ${trustOption.label}`);
    }
    if (approvalValue) {
      const label = findOption(approvalOptions, approvalValue)?.label;
      if (approvalSelectedLabel ?? label) {
        lines.push(`${resolvedApprovalTitle}: ${approvalSelectedLabel ?? label}`);
      }
    }
    if (sandboxValue) {
      lines.push(
        `${t("permissionPicker.sandbox")}: ${
          sandboxSelectedLabel ??
          findOption(sandboxOptions, sandboxValue)?.label ??
          sandboxValue
        }`,
      );
    }
    if (networkValue) {
      lines.push(
        `${t("permissionPicker.network")}: ${
          findOption(networkOptions, networkValue)?.label ?? networkValue
        }`,
      );
    }
    return lines;
  }, [
    approvalOptions,
    approvalSelectedLabel,
    approvalValue,
    networkOptions,
    networkValue,
    resolvedApprovalTitle,
    sandboxOptions,
    sandboxSelectedLabel,
    sandboxValue,
    t,
    trustOption,
    trustScopeLabel,
  ]);

  useLayoutEffect(() => {
    if (!open || !triggerRef.current) {
      return;
    }

    const rect = triggerRef.current.getBoundingClientRect();
    const left = Math.max(8, Math.min(rect.left, window.innerWidth - 460));

    setPos({
      bottom: window.innerHeight - rect.top + 6,
      left,
    });
  }, [open, view]);

  useEffect(() => {
    if (!open) {
      return;
    }

    function onPointerDown(event: PointerEvent) {
      const target = event.target as Node;
      if (
        triggerRef.current?.contains(target) ||
        popoverRef.current?.contains(target)
      ) {
        return;
      }
      setOpen(false);
    }

    function onKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        setOpen(false);
      }
    }

    document.addEventListener("pointerdown", onPointerDown, true);
    document.addEventListener("keydown", onKeyDown, true);

    return () => {
      document.removeEventListener("pointerdown", onPointerDown, true);
      document.removeEventListener("keydown", onKeyDown, true);
    };
  }, [open]);

  const toggle = useCallback(() => {
    if (disabled) {
      return;
    }
    setOpen((prev) => !prev);
  }, [disabled]);

  const presetLabel = useCallback(
    (preset: AutonomyPresetId) => t(`autonomy.presets.${preset}.label`),
    [t],
  );

  const title = summaryLines.length > 0 ? summaryLines.join(" | ") : t("permissionPicker.title");
  const activeItem = railItems.find((item) => item.id === activeSection) ?? null;
  const showCustomPill = presetsAvailable
    ? presetValue === null
    : customPolicyCount > 0;
  const defaultPresetSelected = isDefaultAutonomyPreset(presetValue, defaultPreset);
  const defaultPresetDisabled =
    presetValue == null || (presetValue === "inherit" && defaultPresetSelected);

  const presetPanel = presetsAvailable && engineId && (
    <div className="pp-panel">
      <div className="pp-panel-header">
        <div className="pp-panel-title">
          <span>{t("autonomy.sectionTitle")}</span>
        </div>
        {showCustomPill ? (
          <span className="pp-header-badge">{t("autonomy.custom")}</span>
        ) : null}
      </div>
      <div className="pp-panel-content">
        <div className="pp-options">
          {availableAutonomyPresets(engineId).map((preset) => {
            const selected = preset === presetValue;
            return (
              <button
                key={preset}
                type="button"
                className={`pp-option pp-preset-option${selected ? " pp-option-selected" : ""}`}
                onClick={() => onPresetChange?.(preset)}
              >
                <span className="pp-preset-icon">{PRESET_ICONS[preset]}</span>
                <div className="pp-option-copy">
                  <span className="pp-option-label">{presetLabel(preset)}</span>
                  <span className="pp-option-description">
                    {t(
                      autonomyPresetDescriptionKey(preset, engineId, {
                        codexExternalSandbox,
                      }),
                    )}
                  </span>
                </div>
                {selected ? <Check size={13} className="pp-option-check" /> : null}
              </button>
            );
          })}
        </div>
      </div>
      <div className="pp-preset-footer">
        {onDefaultPresetChange ? (
          <label
            className={`pp-default-toggle${defaultPresetDisabled ? " pp-default-toggle-disabled" : ""}`}
          >
            <input
              type="checkbox"
              checked={defaultPresetSelected}
              disabled={defaultPresetDisabled}
              onChange={(event) => {
                if (presetValue == null) {
                  return;
                }
                onDefaultPresetChange(event.target.checked ? presetValue : null);
              }}
            />
            {t("autonomy.defaultForNewThreads")}
          </label>
        ) : (
          <span />
        )}
        {railItems.length > 0 ? (
          <button
            type="button"
            className="pp-advanced-btn"
            onClick={() => setView("advanced")}
          >
            {t("autonomy.advanced")}
            <ChevronDown size={10} style={{ transform: "rotate(-90deg)" }} />
          </button>
        ) : null}
      </div>
    </div>
  );

  const advancedPanel = (
    <>
      <div className="pp-rail">
        {presetsAvailable ? (
          <button
            type="button"
            className="pp-rail-item pp-rail-back"
            onClick={() => setView("presets")}
          >
            <span className="pp-rail-item-icon">
              <ChevronLeft size={13} />
            </span>
            <span className="pp-rail-item-name">{t("autonomy.backToPresets")}</span>
          </button>
        ) : (
          <div className="pp-rail-label">{t("permissionPicker.policy")}</div>
        )}
        {railItems.map((item) => (
          <button
            key={item.id}
            type="button"
            className={`pp-rail-item${activeSection === item.id ? " pp-rail-item-active" : ""}`}
            onClick={() => setActiveSection(item.id)}
          >
            <span className="pp-rail-item-icon">{item.icon}</span>
            <span className="pp-rail-item-name">{item.title}</span>
          </button>
        ))}
      </div>

      {activeItem ? (
        <div className="pp-panel">
          <div className="pp-panel-header">
            <div className="pp-panel-title">
              <span>{activeItem.title}</span>
            </div>
            {customPolicyCount > 0 ? (
              <span className="pp-header-badge">{t("permissionPicker.custom")}</span>
            ) : null}
          </div>
          <div className="pp-panel-content">
            <div className="pp-options">
              {activeItem.options.map((option) => {
                const selected = option.value === activeItem.value;
                return (
                  <button
                    key={option.value}
                    type="button"
                    className={`pp-option${selected ? " pp-option-selected" : ""}`}
                    onClick={() => activeItem.onChange?.(option.value)}
                    disabled={activeItem.disabled}
                  >
                    <div className="pp-option-copy">
                      <span className="pp-option-label">{option.label}</span>
                      {option.description ? (
                        <span className="pp-option-description">{option.description}</span>
                      ) : null}
                    </div>
                    {selected ? <Check size={13} className="pp-option-check" /> : null}
                  </button>
                );
              })}
            </div>
            {activeItem.note ? <p className="pp-section-note">{activeItem.note}</p> : null}
          </div>
        </div>
      ) : null}
    </>
  );

  const popover = open
    ? createPortal(
        <div
          ref={popoverRef}
          className={`pp-popover${view === "presets" && presetsAvailable ? " pp-popover-presets" : ""}`}
          style={{
            position: "fixed",
            bottom: pos.bottom,
            left: pos.left,
          }}
        >
          {view === "presets" && presetsAvailable ? presetPanel : advancedPanel}
        </div>,
        document.body,
      )
    : null;

  const triggerLabel = presetsAvailable
    ? presetValue != null
      ? presetLabel(presetValue)
      : t("autonomy.custom")
    : trustOption
      ? trustOption.label
      : t("permissionPicker.title");

  return (
    <div className="pp-root">
      <button
        ref={triggerRef}
        type="button"
        className={`pp-trigger${open ? " pp-trigger-open" : ""}`}
        onClick={toggle}
        disabled={disabled}
        title={title}
      >
        <span className="pp-trigger-icon">
          {presetsAvailable && presetValue != null
            ? PRESET_TRIGGER_ICONS[presetValue]
            : <Shield size={12} />}
        </span>
        <span className="pp-trigger-label">{triggerLabel}</span>
        {showCustomPill ? (
          <span className="pp-trigger-pill pp-trigger-pill-accent">
            {presetsAvailable ? t("autonomy.custom") : t("permissionPicker.custom")}
          </span>
        ) : null}
        <ChevronDown
          size={10}
          className={`pp-trigger-chevron${open ? " pp-trigger-chevron-open" : ""}`}
        />
      </button>
      {popover}
    </div>
  );
}
*/

type UnifiedPermissionOption = {
  value: string;
  label: string;
  description?: string;
};

const PERMISSION_OPTIONS: Record<string, UnifiedPermissionOption[]> = {
  autonomyPreset: [
    { value: "automatic", label: "自动" },
    { value: "read-only", label: "只读" },
    { value: "ask", label: "操作前询问" },
    { value: "auto", label: "工作区内自动" },
    { value: "full", label: "完全自主" },
  ],
  trust: [
    { value: "automatic", label: "自动" },
    { value: "trusted", label: "受信任" },
    { value: "standard", label: "标准" },
    { value: "restricted", label: "受限" },
  ],
  approval: [
    { value: "automatic", label: "自动" },
    { value: "restricted", label: "只读" },
    { value: "ask", label: "操作前询问" },
    { value: "autonomous", label: "完全自主" },
  ],
  sandbox: [
    { value: "automatic", label: "自动" },
    { value: "read-only", label: "只读" },
    { value: "workspace-write", label: "工作区可写" },
    { value: "full-access", label: "完全访问" },
  ],
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
