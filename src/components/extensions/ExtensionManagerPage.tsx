import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  AlertCircle,
  ArrowLeft,
  Boxes,
  Check,
  CheckCircle2,
  ChevronRight,
  CircleOff,
  Clock3,
  Loader2,
  Package,
  Plug,
  Puzzle,
  RefreshCw,
  Search,
  ShieldAlert,
  X,
} from "lucide-react";
import type {
  ExtensionAction,
  ExtensionItem,
  ExtensionKind,
  ExtensionProviderId,
} from "../../types";
import {
  buildExtensionCacheKey,
  isAvailableExtension,
  isInstalledExtension,
  resolveExtensionProvider,
  useExtensionStore,
} from "../../stores/extensionStore";
import { useThreadStore } from "../../stores/threadStore";
import { useUiStore } from "../../stores/uiStore";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import { handleDragDoubleClick, handleDragMouseDown } from "../../lib/windowDrag";
import { ConfirmDialog } from "../shared/ConfirmDialog";
import {
  getExtensionCategory,
  getExtensionCategoryOptions,
  groupExtensionItemsByCategory,
  type ExtensionCategoryDescriptor,
} from "./extensionCategories";
import { formatRefreshAge, nextRefreshAgeUpdateDelay } from "./refreshAge";

type StatusFilter = "all" | "installed" | "available" | "disabled" | "issues";

const PROVIDERS: Array<{ id: ExtensionProviderId; label: string }> = [
  { id: "codex", label: "Codex" },
  { id: "claude", label: "Claude Code" },
  { id: "opencode", label: "OpenCode" },
];
const KINDS: ExtensionKind[] = ["skill", "plugin", "mcp"];
const DESTRUCTIVE_ACTIONS = new Set<ExtensionAction>(["uninstall", "remove"]);

function useRefreshAgeClock(timestamp: string | null | undefined): number {
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    if (!timestamp) return;
    let timer: number | undefined;
    const scheduleNextUpdate = () => {
      const updatedNow = Date.now();
      setNow(updatedNow);
      const delay = nextRefreshAgeUpdateDelay(timestamp, updatedNow);
      if (delay !== null) {
        timer = window.setTimeout(scheduleNextUpdate, delay);
      }
    };
    scheduleNextUpdate();
    return () => {
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [timestamp]);

  return now;
}

function ExtensionKindIcon({ kind, size = 16 }: { kind: ExtensionKind; size?: number }) {
  if (kind === "skill") return <Puzzle size={size} />;
  if (kind === "plugin") return <Package size={size} />;
  return <Plug size={size} />;
}

function itemMatchesStatus(item: ExtensionItem, filter: StatusFilter): boolean {
  if (filter === "installed") return isInstalledExtension(item);
  if (filter === "available") return isAvailableExtension(item);
  if (filter === "disabled") return isInstalledExtension(item) && item.enabled === false;
  if (filter === "issues") {
    return ["disconnected", "auth_required", "error"].includes(item.health);
  }
  return isInstalledExtension(item) || isAvailableExtension(item);
}

function statusKey(item: ExtensionItem): string {
  if (isAvailableExtension(item)) return "available";
  if (item.enabled === false) return "disabled";
  if (item.health === "auth_required") return "authRequired";
  if (item.health === "disconnected") return "disconnected";
  if (item.health === "error") return "error";
  if (item.kind === "mcp") return item.health === "healthy" ? "connected" : "configured";
  if (isInstalledExtension(item)) return item.enabled === true ? "enabled" : "installed";
  return "available";
}

function ExtensionCard({
  item,
  selected,
  onSelect,
}: {
  item: ExtensionItem;
  selected: boolean;
  onSelect: () => void;
}) {
  const { t } = useTranslation("extensions");
  const issue = ["disconnected", "auth_required", "error"].includes(item.health);
  return (
    <button
      type="button"
      className={`em-card${selected ? " em-card-selected" : ""}`}
      onClick={onSelect}
      aria-pressed={selected}
    >
      <span className={`em-card-icon em-card-icon-${item.kind}`}>
        <ExtensionKindIcon kind={item.kind} />
      </span>
      <span className="em-card-body">
        <span className="em-card-title-row">
          <span className="em-card-name">{item.name}</span>
          <span className={`em-status${issue ? " em-status-issue" : ""}`}>
            {issue ? <AlertCircle size={11} /> : <Check size={11} />}
            {t(`status.${statusKey(item)}`)}
          </span>
        </span>
        <span className="em-card-description">
          {item.description?.trim() || t("item.noDescription")}
        </span>
        <span className="em-card-meta">
          <span>{t(`scope.${item.scope}`, { defaultValue: item.scope })}</span>
          {item.version && <span>{item.version}</span>}
          {(item.marketplace || item.source) && <span>{item.marketplace || item.source}</span>}
        </span>
      </span>
      <ChevronRight className="em-card-chevron" size={15} />
    </button>
  );
}

export function ExtensionManagerPage() {
  const { t, i18n } = useTranslation(["extensions", "workspace", "common"]);
  const setActiveView = useUiStore((state) => state.setActiveView);
  const { workspaces, activeWorkspaceId, repos, activeRepoId } = useWorkspaceStore();
  const { threads, activeThreadId } = useThreadStore();
  const entries = useExtensionStore((state) => state.entries);
  const selectedProviderByWorkspace = useExtensionStore(
    (state) => state.selectedProviderByWorkspace,
  );
  const setSelectedProvider = useExtensionStore((state) => state.setSelectedProvider);
  const loadCatalog = useExtensionStore((state) => state.loadCatalog);
  const requestRefresh = useExtensionStore((state) => state.requestRefresh);
  const performAction = useExtensionStore((state) => state.performAction);

  const workspace = workspaces.find((candidate) => candidate.id === activeWorkspaceId) ?? null;
  const repo = repos.find((candidate) => candidate.id === activeRepoId) ?? null;
  const activeThread = threads.find((candidate) => candidate.id === activeThreadId) ?? null;
  const workspaceKey = activeWorkspaceId ?? "global";
  const preferredProvider = resolveExtensionProvider(
    activeWorkspaceId,
    selectedProviderByWorkspace,
    activeThread?.workspaceId === activeWorkspaceId ? activeThread.engineId : null,
    "codex",
  );
  const [providerId, setProviderId] = useState<ExtensionProviderId>(preferredProvider);
  const previousWorkspaceRef = useRef(workspaceKey);
  const detailsCloseRef = useRef<HTMLButtonElement>(null);
  const cwd = repo?.path ?? workspace?.rootPath ?? null;
  const context = useMemo(
    () => ({ providerId, workspaceId: activeWorkspaceId, repoId: activeRepoId, cwd }),
    [activeRepoId, activeWorkspaceId, cwd, providerId],
  );
  const cacheKey = buildExtensionCacheKey(context);
  const entry = entries[cacheKey];
  const catalog = entry?.catalog;

  const [kind, setKind] = useState<ExtensionKind>("skill");
  const kindFetchedAt = catalog?.kindFetchedAt?.[kind] ?? null;
  const refreshAgeNow = useRefreshAgeClock(kindFetchedAt);
  const refreshAge = formatRefreshAge(kindFetchedAt, i18n.language, refreshAgeNow);
  const [statusFilter, setStatusFilter] = useState<StatusFilter>("all");
  const [query, setQuery] = useState("");
  const [source, setSource] = useState("all");
  const [scope, setScope] = useState<ExtensionItem["scope"] | "all">("all");
  const [categoryFilter, setCategoryFilter] = useState("all");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [pendingConfirmation, setPendingConfirmation] = useState<{
    item: ExtensionItem;
    action: ExtensionAction;
    dependencies: string[];
  } | null>(null);

  useEffect(() => {
    if (previousWorkspaceRef.current === workspaceKey) return;
    previousWorkspaceRef.current = workspaceKey;
    setProviderId(preferredProvider);
    setSelectedId(null);
  }, [preferredProvider, workspaceKey]);

  useEffect(() => {
    void loadCatalog(context);
  }, [context, loadCatalog]);

  useEffect(() => {
    setSelectedId(null);
    setSource("all");
    setScope("all");
    setCategoryFilter("all");
  }, [kind, providerId]);

  const categoryLabel = useCallback(
    (category: ExtensionCategoryDescriptor) =>
      t(`extensions:${category.translationKey}`, { defaultValue: category.defaultLabel }),
    [t],
  );

  const counts = useMemo(
    () =>
      Object.fromEntries(
        KINDS.map((candidateKind) => [
          candidateKind,
          catalog?.items.filter(
            (item) =>
              item.kind === candidateKind &&
              (isInstalledExtension(item) || isAvailableExtension(item)),
          ).length ?? 0,
        ]),
      ) as Record<ExtensionKind, number>,
    [catalog],
  );

  const categoryOptions = useMemo(() => {
    const items = (catalog?.items ?? []).filter(
      (item) =>
        item.kind === kind &&
        (isInstalledExtension(item) || isAvailableExtension(item)),
    );
    return getExtensionCategoryOptions(items).sort((left, right) =>
      categoryLabel(left).localeCompare(categoryLabel(right)),
    );
  }, [catalog, categoryLabel, kind]);
  const activeCategoryFilter =
    categoryFilter === "all" || categoryOptions.some((option) => option.id === categoryFilter)
      ? categoryFilter
      : "all";

  const filterableKindItems = useMemo(
    () =>
      (catalog?.items ?? []).filter(
        (item) =>
          item.kind === kind &&
          (isInstalledExtension(item) || isAvailableExtension(item)),
      ),
    [catalog, kind],
  );
  const scopes = useMemo(
    () => [...new Set(filterableKindItems.map((item) => item.scope))].sort(),
    [filterableKindItems],
  );
  const sources = useMemo(
    () =>
      [
        ...new Set(
          filterableKindItems
            .flatMap((item) => [item.marketplace, item.source])
            .filter((value): value is string => Boolean(value)),
        ),
      ].sort(),
    [filterableKindItems],
  );
  const activeSource = source === "all" || sources.includes(source) ? source : "all";
  const activeScope = scope === "all" || scopes.includes(scope) ? scope : "all";

  const kindItems = useMemo(() => {
    const normalizedQuery = query.trim().toLocaleLowerCase();
    return (catalog?.items ?? []).filter((item) => {
      if (item.kind !== kind || !itemMatchesStatus(item, statusFilter)) return false;
      if (
        activeCategoryFilter !== "all" &&
        getExtensionCategory(item).id !== activeCategoryFilter
      ) {
        return false;
      }
      if (
        activeSource !== "all" &&
        item.source !== activeSource &&
        item.marketplace !== activeSource
      ) {
        return false;
      }
      if (activeScope !== "all" && item.scope !== activeScope) return false;
      if (!normalizedQuery) return true;
      return [item.name, item.description, item.source, item.marketplace, item.category]
        .filter(Boolean)
        .some((value) => value!.toLocaleLowerCase().includes(normalizedQuery));
    });
  }, [
    activeCategoryFilter,
    activeScope,
    activeSource,
    catalog,
    kind,
    query,
    statusFilter,
  ]);

  const installedItems = kindItems.filter(isInstalledExtension);
  const availableItems = kindItems.filter(isAvailableExtension);
  const availableCategoryGroups = groupExtensionItemsByCategory(availableItems)
    .map((group) => ({ ...group, label: categoryLabel(group.category) }))
    .sort((left, right) => left.label.localeCompare(right.label));
  const selectedItem =
    kindItems.find((item) => `${item.kind}:${item.id}` === selectedId) ?? null;
  const selectedCategory = selectedItem ? getExtensionCategory(selectedItem) : null;
  const relatedItems = selectedItem
    ? (catalog?.items ?? []).filter((item) => item.parentPluginId === selectedItem.id)
    : [];
  const runAction = useCallback(
    async (item: ExtensionItem, action: ExtensionAction) => {
      try {
        const result = await performAction(context, item, action, item.scope);
        const { toast } = await import("../../stores/toastStore");
        toast.success(
          result.requiresNewSession
            ? t("extensions:actions.completedNewSession")
            : t("extensions:actions.completed"),
        );
      } catch (error) {
        const { toast } = await import("../../stores/toastStore");
        toast.error(String(error));
      }
    },
    [context, performAction, t],
  );

  useEffect(() => {
    if (!selectedItem) return;
    const timer = window.setTimeout(() => detailsCloseRef.current?.focus(), 0);
    function onKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.stopPropagation();
        setSelectedId(null);
      }
    }
    window.addEventListener("keydown", onKeyDown, true);
    return () => {
      window.clearTimeout(timer);
      window.removeEventListener("keydown", onKeyDown, true);
    };
  }, [selectedItem]);

  function requestAction(item: ExtensionItem, action: ExtensionAction) {
    if (DESTRUCTIVE_ACTIONS.has(action)) {
      setPendingConfirmation({
        item,
        action,
        dependencies: (catalog?.items ?? [])
          .filter((candidate) => candidate.parentPluginId === item.id)
          .map((candidate) => candidate.name),
      });
      return;
    }
    void runAction(item, action);
  }

  function changeProvider(next: ExtensionProviderId) {
    setProviderId(next);
    setSelectedProvider(activeWorkspaceId, next);
    setSelectedId(null);
  }

  const loading =
    entry?.phase === "loading" || entry?.refreshRequested === true || catalog?.refreshing === true;
  const initialLoading = loading && !catalog;
  const capabilities = catalog?.capabilities;
  const readOnlyReason = selectedItem?.readOnlyReason
    ? t(`extensions:readOnly.${selectedItem.readOnlyReason}`, {
        defaultValue: selectedItem.readOnlyReason,
      })
    : null;
  const hasOfficialCatalog =
    kind === "skill"
      ? capabilities?.hasOfficialSkillCatalog
      : kind === "plugin"
        ? capabilities?.hasOfficialPluginCatalog
        : capabilities?.hasOfficialMcpCatalog;

  return (
    <div className="em-root">
      <div className="em-scroll">
        <main className="em-inner">
          <header
            className="em-header"
            onMouseDown={handleDragMouseDown}
            onDoubleClick={handleDragDoubleClick}
          >
            <div className="em-heading">
              <button
                type="button"
                className="em-icon-button"
                onClick={() => setActiveView("chat")}
                title={t("workspace:actions.back")}
              >
                <ArrowLeft size={15} />
              </button>
              <span className="em-heading-icon"><Boxes size={17} /></span>
              <div>
                <h1>{t("extensions:title")}</h1>
                <p>{t("extensions:subtitle")}</p>
              </div>
            </div>
            <div className="em-context-controls" onMouseDown={(event) => event.stopPropagation()}>
              <label className="em-select-label">
                <span>{t("extensions:provider.label")}</span>
                <select
                  value={providerId}
                  onChange={(event) => changeProvider(event.target.value as ExtensionProviderId)}
                  aria-label={t("extensions:provider.label")}
                >
                  {PROVIDERS.map((provider) => (
                    <option key={provider.id} value={provider.id}>{provider.label}</option>
                  ))}
                </select>
              </label>
              <span className="em-workspace-context" title={cwd ?? undefined}>
                {workspace ? repo?.name ?? workspace.name : t("extensions:workspace.none")}
              </span>
              <button
                type="button"
                className="em-icon-button"
                onClick={() => void requestRefresh(context)}
                disabled={loading}
                title={t("extensions:actions.refresh")}
                aria-label={t("extensions:actions.refresh")}
              >
                <RefreshCw size={14} className={loading ? "em-spin" : undefined} />
              </button>
            </div>
          </header>

          {!workspace && (
            <div className="em-notice"><CircleOff size={14} />{t("extensions:workspace.hint")}</div>
          )}

          <div className="em-search-wrap">
            <Search size={16} />
            <input
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder={t("extensions:search.placeholder")}
              aria-label={t("extensions:search.placeholder")}
            />
            {query && (
              <button type="button" onClick={() => setQuery("")} aria-label={t("common:actions.close")}>
                <X size={14} />
              </button>
            )}
          </div>

          <div className="em-tabs" role="tablist" aria-label={t("extensions:tabs.label")}>
            {KINDS.map((candidateKind) => (
              <button
                key={candidateKind}
                type="button"
                role="tab"
                aria-selected={kind === candidateKind}
                className={kind === candidateKind ? "em-tab-active" : undefined}
                onClick={() => setKind(candidateKind)}
              >
                <ExtensionKindIcon kind={candidateKind} size={14} />
                {t(`extensions:tabs.${candidateKind}`)}
                <span>{counts[candidateKind]}</span>
              </button>
            ))}
          </div>

          <div className="em-filters">
            <div className="em-filter-chips">
              {(["all", "installed", "available", "disabled", "issues"] as StatusFilter[]).map(
                (filter) => (
                  <button
                    type="button"
                    key={filter}
                    className={statusFilter === filter ? "em-filter-active" : undefined}
                    onClick={() => setStatusFilter(filter)}
                  >
                    {t(`extensions:filters.${filter}`)}
                  </button>
                ),
              )}
            </div>
            <div className="em-filter-selects">
              <select value={activeSource} onChange={(event) => setSource(event.target.value)}>
                <option value="all">{t("extensions:filters.allSources")}</option>
                {sources.map((value) => <option key={value} value={value}>{value}</option>)}
              </select>
              <select
                value={activeScope}
                onChange={(event) => setScope(event.target.value as ExtensionItem["scope"] | "all")}
              >
                <option value="all">{t("extensions:filters.allScopes")}</option>
                {scopes.map((value) => (
                  <option key={value} value={value}>
                    {t(`extensions:scope.${value}`, { defaultValue: value })}
                  </option>
                ))}
              </select>
            </div>
          </div>

          {categoryOptions.length > 0 && (
            <div
              className="em-category-filter"
              role="group"
              aria-label={t("extensions:categories.label")}
            >
              <span className="em-category-label">{t("extensions:categories.label")}</span>
              <div className="em-category-chips">
                <button
                  type="button"
                  className={activeCategoryFilter === "all" ? "em-category-active" : undefined}
                  aria-pressed={activeCategoryFilter === "all"}
                  onClick={() => setCategoryFilter("all")}
                >
                  {t("extensions:categories.all")}
                </button>
                {categoryOptions.map((category) => (
                  <button
                    type="button"
                    key={category.id}
                    className={
                      activeCategoryFilter === category.id ? "em-category-active" : undefined
                    }
                    aria-pressed={activeCategoryFilter === category.id}
                    onClick={() => setCategoryFilter(category.id)}
                  >
                    {categoryLabel(category)}
                  </button>
                ))}
              </div>
            </div>
          )}

          {catalog?.refreshErrors?.map((error) => (
            <div className="em-warning" key={`${error.kind}:${error.code}`}>
              <ShieldAlert size={14} />
              {t(`extensions:refreshErrors.${error.code}`, {
                kind: t(`extensions:tabs.${error.kind}`),
                defaultValue: t("extensions:refreshErrors.refreshFailed", {
                  kind: t(`extensions:tabs.${error.kind}`),
                }),
              })}
            </div>
          ))}
          {catalog?.hasSnapshot === false && entry?.phase !== "error" && (
            <div className="em-notice">
              <CircleOff size={14} />
              <span>{t("extensions:cache.notFetched")}</span>
              <button type="button" onClick={() => void requestRefresh(context)}>
                {t("extensions:cache.requestNow")}
              </button>
            </div>
          )}
          {loading && catalog && (
            <div className="em-notice"><Loader2 size={14} className="em-spin" />{t("extensions:cache.refreshing")}</div>
          )}
          {refreshAge && (
            <div className="em-notice"><Clock3 size={14} />{t("extensions:cache.refreshedAt", { age: refreshAge })}</div>
          )}
          {!loading && catalog?.refreshCompletedAt && (
            <div className="em-notice"><CheckCircle2 size={14} />{t("extensions:cache.refreshFinished")}</div>
          )}
          {entry?.phase === "error" && (
            <div className="em-error">
              <AlertCircle size={15} />
              <span>{entry.error}</span>
              <button type="button" onClick={() => void requestRefresh(context)}>
                {t("extensions:actions.retry")}
              </button>
            </div>
          )}

          {initialLoading ? (
            <div className="em-loading"><Loader2 size={20} className="em-spin" />{t("extensions:loading")}</div>
          ) : (
            <div className="em-sections">
              <section>
                <div className="em-section-heading">
                  <h2>{t("extensions:sections.installed")}</h2>
                  <span>{installedItems.length}</span>
                </div>
                {installedItems.length ? (
                  <div className="em-grid">
                    {installedItems.map((item) => {
                      const id = `${item.kind}:${item.id}`;
                      return (
                        <ExtensionCard
                          key={id}
                          item={item}
                          selected={selectedId === id}
                          onSelect={() => setSelectedId(id)}
                        />
                      );
                    })}
                  </div>
                ) : (
                  <div className="em-empty">{t("extensions:empty.installed")}</div>
                )}
              </section>

              <section>
                <div className="em-section-heading">
                  <h2>{t("extensions:sections.available")}</h2>
                  <span>{availableItems.length}</span>
                </div>
                {availableItems.length ? (
                  <div className="em-category-groups">
                    {availableCategoryGroups.map((group) => (
                      <div className="em-category-group" key={group.category.id}>
                        <div className="em-category-heading">
                          <h3>{group.label}</h3>
                          <span>{group.items.length}</span>
                        </div>
                        <div className="em-grid">
                          {group.items.map((item) => {
                            const id = `${item.kind}:${item.id}`;
                            return (
                              <ExtensionCard
                                key={id}
                                item={item}
                                selected={selectedId === id}
                                onSelect={() => setSelectedId(id)}
                              />
                            );
                          })}
                        </div>
                      </div>
                    ))}
                  </div>
                ) : (
                  <div className="em-empty">
                    {hasOfficialCatalog === false
                      ? t("extensions:empty.noOfficialCatalog")
                      : t("extensions:empty.available")}
                  </div>
                )}
              </section>
            </div>
          )}
        </main>
      </div>

      {selectedItem && (
        <aside className="em-details" aria-label={t("extensions:details.title")}>
          <div className="em-details-header">
            <span className={`em-card-icon em-card-icon-${selectedItem.kind}`}>
              <ExtensionKindIcon kind={selectedItem.kind} />
            </span>
            <div>
              <h2>{selectedItem.name}</h2>
              <span>{t(`extensions:tabs.${selectedItem.kind}`)} · {providerId}</span>
            </div>
            <button
              ref={detailsCloseRef}
              type="button"
              className="em-icon-button"
              onClick={() => setSelectedId(null)}
              aria-label={t("common:actions.close")}
            >
              <X size={15} />
            </button>
          </div>
          <div className="em-details-body">
            <p className="em-details-description">
              {selectedItem.description?.trim() || t("extensions:item.noDescription")}
            </p>
            <dl>
              <div><dt>{t("extensions:details.status")}</dt><dd>{t(`extensions:status.${statusKey(selectedItem)}`)}</dd></div>
              <div><dt>{t("extensions:details.scope")}</dt><dd>{t(`extensions:scope.${selectedItem.scope}`, { defaultValue: selectedItem.scope })}</dd></div>
              {selectedCategory && (
                <div><dt>{t("extensions:details.category")}</dt><dd>{categoryLabel(selectedCategory)}</dd></div>
              )}
              {selectedItem.version && <div><dt>{t("extensions:details.version")}</dt><dd>{selectedItem.version}</dd></div>}
              {(selectedItem.marketplace || selectedItem.source) && (
                <div><dt>{t("extensions:details.source")}</dt><dd>{selectedItem.marketplace || selectedItem.source}</dd></div>
              )}
              {selectedItem.path && <div><dt>{t("extensions:details.path")}</dt><dd title={selectedItem.path}>{selectedItem.path}</dd></div>}
              {selectedItem.parentPluginId && <div><dt>{t("extensions:details.parentPlugin")}</dt><dd>{selectedItem.parentPluginId}</dd></div>}
            </dl>
            {relatedItems.length > 0 && (
              <div className="em-related-items">
                <h3>{t("extensions:details.included")}</h3>
                <ul>
                  {relatedItems.map((item) => (
                    <li key={`${item.kind}:${item.id}`}>
                      <ExtensionKindIcon kind={item.kind} size={12} />
                      <span>{item.name}</span>
                      <small>{t(`extensions:tabs.${item.kind}`)}</small>
                    </li>
                  ))}
                </ul>
              </div>
            )}
            {selectedItem.requiresNewSession && (
              <div className="em-notice"><RefreshCw size={13} />{t("extensions:details.newSession")}</div>
            )}
            {readOnlyReason && (
              <div className="em-readonly"><CircleOff size={13} />{readOnlyReason}</div>
            )}
          </div>
          <div className="em-details-actions">
            {selectedItem.availableActions.length ? (
              selectedItem.availableActions.map((action) => (
                <button
                  key={action}
                  type="button"
                  className={DESTRUCTIVE_ACTIONS.has(action) ? "em-action-danger" : "em-action-primary"}
                  disabled={Boolean(entry?.action)}
                  onClick={() => requestAction(selectedItem, action)}
                >
                  {entry?.action?.extensionId === selectedItem.id && entry.action.action === action && (
                    <Loader2 size={12} className="em-spin" />
                  )}
                  {t(`extensions:actions.${action}`)}
                </button>
              ))
            ) : (
              <span>{readOnlyReason || t("extensions:actions.none")}</span>
            )}
          </div>
        </aside>
      )}

      <ConfirmDialog
        open={Boolean(pendingConfirmation)}
        title={t("extensions:confirm.title")}
        message={t(
          pendingConfirmation?.dependencies.length
            ? "extensions:confirm.messageWithDependencies"
            : "extensions:confirm.message",
          {
            name: pendingConfirmation?.item.name ?? "",
            dependencies: pendingConfirmation?.dependencies.join(", ") ?? "",
          },
        )}
        confirmLabel={t("extensions:confirm.confirm")}
        onConfirm={() => {
          if (pendingConfirmation) {
            void runAction(pendingConfirmation.item, pendingConfirmation.action);
          }
          setPendingConfirmation(null);
        }}
        onCancel={() => setPendingConfirmation(null)}
      />
    </div>
  );
}
