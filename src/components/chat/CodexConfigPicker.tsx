import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { ChevronDown, SlidersHorizontal } from "lucide-react";
import { useTranslation } from "react-i18next";

export interface CodexConfigPatch {
  /** 是否更新人格选项。 */
  updatePersonality: boolean;
  /** 新人格选项；null 表示继承默认值。 */
  personality: string | null;
  /** 是否更新服务档位。 */
  updateServiceTier: boolean;
  /** 新服务档位；null 表示继承默认值。 */
  serviceTier: string | null;
  /** 是否更新输出结构。 */
  updateOutputSchema: boolean;
  /** 新输出结构；null 表示清除当前配置。 */
  outputSchema: Record<string, unknown> | boolean | null;
}

export type CodexPersonalityValue =
  | "inherit"
  | "none"
  | "friendly"
  | "pragmatic";

export type CodexServiceTierValue = "inherit" | "fast" | "flex";

interface CodexConfigPickerProps {
  /** 当前人格选择。 */
  personalityValue: CodexPersonalityValue;
  /** 当前模型是否支持人格选择。 */
  personalitySupported: boolean;
  /** 当前服务档位选择。 */
  serviceTierValue: CodexServiceTierValue;
  /** 当前输出结构配置。 */
  outputSchemaValue: Record<string, unknown> | boolean | null;
  /** 当前配置中已启用的非权限选项数量。 */
  activeCount: number;
  /** 是否禁用配置编辑。 */
  disabled?: boolean;
  /** 保存 Codex 非权限配置的业务回调。 */
  onSave: (patch: CodexConfigPatch) => Promise<void>;
}

interface DraftState {
  /** 编辑中的人格选择。 */
  personality: CodexPersonalityValue;
  /** 编辑中的服务档位选择。 */
  serviceTier: CodexServiceTierValue;
  /** 编辑中的输出结构 JSON 文本。 */
  outputSchemaText: string;
}

/** 将 Codex 配置值格式化为可编辑的 JSON 文本。 */
function formatJsonEditorValue(value: unknown): string {
  if (value === null || value === undefined) {
    return "";
  }
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return "";
  }
}

/** 将当前配置值组装为编辑器草稿。 */
function buildDraftState(
  personalityValue: CodexPersonalityValue,
  serviceTierValue: CodexServiceTierValue,
  outputSchemaValue: Record<string, unknown> | boolean | null,
): DraftState {
  return {
    personality: personalityValue,
    serviceTier: serviceTierValue,
    outputSchemaText: formatJsonEditorValue(outputSchemaValue),
  };
}

/** 规范化配置文本，用于判断用户是否修改了草稿。 */
function normalizeTextValue(value: string): string {
  return value.trim();
}

/** 编辑并保存 Codex 的人格、服务档位和输出结构配置。 */
export function CodexConfigPicker({
  personalityValue,
  personalitySupported,
  serviceTierValue,
  outputSchemaValue,
  activeCount,
  disabled = false,
  onSave,
}: CodexConfigPickerProps) {
  const { t } = useTranslation("chat");
  const [open, setOpen] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [draft, setDraft] = useState<DraftState>(() =>
    buildDraftState(
      personalityValue,
      serviceTierValue,
      outputSchemaValue,
    ),
  );
  const triggerRef = useRef<HTMLButtonElement>(null);
  const popoverRef = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ bottom: 0, left: 0 });

  const initialDraft = useMemo(
    () =>
      buildDraftState(
        personalityValue,
        serviceTierValue,
        outputSchemaValue,
      ),
    [
      outputSchemaValue,
      personalityValue,
      serviceTierValue,
    ],
  );
  const showPersonalityField =
    personalitySupported ||
    personalityValue !== "inherit" ||
    draft.personality !== "inherit";

  const hasChanges =
    draft.personality !== initialDraft.personality ||
    draft.serviceTier !== initialDraft.serviceTier ||
    normalizeTextValue(draft.outputSchemaText) !==
      normalizeTextValue(initialDraft.outputSchemaText);

  useEffect(() => {
    if (!open) {
      setError(null);
      return;
    }

    setDraft(initialDraft);
    setError(null);
  }, [initialDraft, open]);

  useLayoutEffect(() => {
    if (!open || !triggerRef.current) {
      return;
    }

    const rect = triggerRef.current.getBoundingClientRect();
    const left = Math.max(8, Math.min(rect.left, window.innerWidth - 440));
    setPos({
      bottom: window.innerHeight - rect.top + 6,
      left,
    });
  }, [open]);

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

    window.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [open]);

  /** 校验当前非权限配置草稿并提交保存。 */
  async function handleSave() {
    if (!hasChanges) {
      setOpen(false);
      return;
    }

    let outputSchema: Record<string, unknown> | boolean | null = null;

    try {
      const outputSchemaText = normalizeTextValue(draft.outputSchemaText);
      if (outputSchemaText) {
        const parsed = JSON.parse(outputSchemaText) as unknown;
        if (
          typeof parsed !== "boolean" &&
          (!parsed || typeof parsed !== "object" || Array.isArray(parsed))
        ) {
          setError(
            t("configPicker.invalidOutputSchema", {
              error: t("configPicker.invalidOutputSchemaType"),
            }),
          );
          return;
        }
        outputSchema = parsed as Record<string, unknown> | boolean;
      }

    } catch (parseError) {
      setError(t("configPicker.invalidJson", { error: String(parseError) }));
      return;
    }

    setSaving(true);
    setError(null);
    try {
      await onSave({
        updatePersonality: draft.personality !== initialDraft.personality,
        personality: draft.personality === "inherit" ? null : draft.personality,
        updateServiceTier: draft.serviceTier !== initialDraft.serviceTier,
        serviceTier: draft.serviceTier === "inherit" ? null : draft.serviceTier,
        updateOutputSchema:
          normalizeTextValue(draft.outputSchemaText) !==
          normalizeTextValue(initialDraft.outputSchemaText),
        outputSchema,
      });
      setOpen(false);
    } catch (saveError) {
      setError(String(saveError));
    } finally {
      setSaving(false);
    }
  }

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        className={`chat-toolbar-btn chat-toolbar-btn-bordered${open ? " chat-toolbar-btn-active" : ""}`}
        disabled={disabled}
        title={t("configPicker.title")}
        onClick={() => setOpen((current) => !current)}
      >
        <SlidersHorizontal size={12} />
        <span style={{ fontSize: 11 }}>{t("configPicker.shortTitle")}</span>
        {activeCount > 0 && (
          <span className="chat-toolbar-badge">{activeCount}</span>
        )}
        <ChevronDown size={12} />
      </button>

      {open &&
        createPortal(
          <div
            ref={popoverRef}
            className="codex-config-popover"
            style={{
              position: "fixed",
              zIndex: 1300,
              bottom: pos.bottom,
              left: pos.left,
            }}
          >
            <div className="codex-config-header">
              <div>
                <div className="codex-config-title">{t("configPicker.title")}</div>
                <div className="codex-config-subtitle">
                  {t("configPicker.subtitle")}
                </div>
              </div>
              {activeCount > 0 ? (
                <span className="codex-config-count">
                  {t("configPicker.customCount", { count: activeCount })}
                </span>
              ) : null}
            </div>

            <div className="codex-config-fields">
              {showPersonalityField ? (
                <label className="codex-config-field">
                  <span className="codex-config-label">
                    {t("configPicker.personality")}
                  </span>
                  <select
                    className="codex-config-select"
                    value={draft.personality}
                    onChange={(event) =>
                      setDraft((current) => ({
                        ...current,
                        personality: event.target.value as CodexPersonalityValue,
                      }))
                    }
                    disabled={saving}
                  >
                    <option value="inherit">
                      {t("configPicker.inherit")}
                    </option>
                    <option value="none">
                      {t("configPicker.personalities.none")}
                    </option>
                    <option value="friendly">
                      {t("configPicker.personalities.friendly")}
                    </option>
                    <option value="pragmatic">
                      {t("configPicker.personalities.pragmatic")}
                    </option>
                  </select>
                  <span className="codex-config-note">
                    {personalitySupported
                      ? t("configPicker.personalityDescription")
                      : t("configPicker.personalityUnsupported")}
                  </span>
                </label>
              ) : null}

              <label className="codex-config-field">
                <span className="codex-config-label">
                  {t("configPicker.serviceTier")}
                </span>
                <select
                  className="codex-config-select"
                  value={draft.serviceTier}
                  onChange={(event) =>
                    setDraft((current) => ({
                      ...current,
                      serviceTier: event.target.value as CodexServiceTierValue,
                    }))
                  }
                  disabled={saving}
                >
                  <option value="inherit">
                    {t("configPicker.inherit")}
                  </option>
                  <option value="fast">
                    {t("configPicker.serviceTiers.fast")}
                  </option>
                  <option value="flex">
                    {t("configPicker.serviceTiers.flex")}
                  </option>
                </select>
                <span className="codex-config-note">
                  {t("configPicker.serviceTierDescription")}
                </span>
              </label>

              <label className="codex-config-field">
                <span className="codex-config-label">
                  {t("configPicker.outputSchema")}
                </span>
                <textarea
                  className="codex-config-textarea"
                  value={draft.outputSchemaText}
                  onChange={(event) =>
                    setDraft((current) => ({
                      ...current,
                      outputSchemaText: event.target.value,
                    }))
                  }
                  placeholder={t("configPicker.outputSchemaPlaceholder")}
                  rows={6}
                  spellCheck={false}
                  disabled={saving}
                />
                <span className="codex-config-note">
                  {t("configPicker.outputSchemaDescription")}
                </span>
              </label>

            </div>

            {error ? <div className="codex-config-error">{error}</div> : null}

            <div className="codex-config-actions">
              <button
                type="button"
                className="chat-toolbar-btn chat-toolbar-btn-bordered"
                onClick={() => setDraft(initialDraft)}
                disabled={saving}
              >
                {t("configPicker.reset")}
              </button>
              <button
                type="button"
                className="chat-toolbar-btn chat-toolbar-btn-active"
                onClick={() => void handleSave()}
                disabled={saving}
              >
                {saving ? t("configPicker.saving") : t("configPicker.save")}
              </button>
            </div>
          </div>,
          document.body,
        )}
    </>
  );
}
