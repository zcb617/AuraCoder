import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import MarkdownContent from "./MarkdownContent";

const renderMarkdownToHtmlMock = vi.hoisted(() =>
  vi.fn((content: string) =>
    content.includes("```")
      ? `<pre><code class="hljs">${content}</code></pre>`
      : `<p>${content}</p>`,
  ),
);

const hookHarness = vi.hoisted(() => {
  type HookSlot = {
    // 保存当前 Hook 类型，确保测试渲染顺序与组件业务顺序一致。
    kind: "state" | "ref" | "memo" | "effect";
    // 保存 useState、useRef 或 useMemo 对应的业务值。
    value: unknown;
    // 保存 useMemo 或 useEffect 的依赖，决定是否重新计算业务结果。
    deps: readonly unknown[] | null;
    // 保存 useEffect 返回的业务清理逻辑。
    cleanup?: () => void;
    // 保存待执行的业务副作用。
    effect?: () => void | (() => void);
    // 标记本轮渲染后是否需要执行业务副作用。
    pending?: boolean;
    // 保存下一轮 useEffect 将采用的业务依赖。
    nextDeps?: readonly unknown[] | null;
  };

  const slots: HookSlot[] = [];
  let cursor = 0;

  // 为每次组件渲染重置 Hook 游标，模拟 React 保持 Hook 状态的业务行为。
  const beginRender = () => {
    cursor = 0;
  };

  // 按 Hook 顺序复用测试状态槽，模拟组件重渲染时读取同一业务状态。
  const getSlot = (kind: HookSlot["kind"], value: unknown): HookSlot => {
    const slot = slots[cursor] ?? {
      kind,
      value,
      deps: null,
    };
    if (slot.kind !== kind) {
      throw new Error(`Hook 顺序异常：期望 ${kind}，实际 ${slot.kind}`);
    }
    slots[cursor] = slot;
    cursor += 1;
    return slot;
  };

  // 保存并更新组件 useState 的业务值，供定时器触发后重新渲染。
  const useState = <T,>(initialValue: T): [T, (next: T | ((previous: T) => T)) => void] => {
    const slot = getSlot("state", initialValue);
    const setValue = (next: T | ((previous: T) => T)) => {
      slot.value = typeof next === "function"
        ? (next as (previous: T) => T)(slot.value as T)
        : next;
    };
    return [slot.value as T, setValue];
  };

  // 保存组件 useRef 的业务引用，保证流式 HTML 与定时器跨渲染保留。
  const useRef = <T,>(initialValue: T): { current: T } => {
    const slot = getSlot("ref", { current: initialValue });
    return slot.value as { current: T };
  };

  // 按依赖变化计算组件 useMemo 的业务结果，模拟 React 的记忆化渲染。
  const useMemo = <T,>(factory: () => T, deps: readonly unknown[]): T => {
    const slot = getSlot("memo", undefined);
    const previousDeps = slot.deps;
    const dependenciesUnchanged = previousDeps !== null &&
      previousDeps.length === deps.length &&
      previousDeps.every((dependency, index) => Object.is(dependency, deps[index]));
    if (!dependenciesUnchanged) {
      slot.value = factory();
      slot.deps = [...deps];
    }
    return slot.value as T;
  };

  // 登记组件副作用及其依赖，模拟 React 在提交后调度流式刷新定时器。
  const useEffect = (effect: () => void | (() => void), deps?: readonly unknown[]) => {
    const slot = getSlot("effect", undefined);
    const nextDeps = deps === undefined ? null : [...deps];
    const previousDeps = slot.deps;
    const dependenciesChanged = previousDeps === null ||
      nextDeps === null ||
      previousDeps.length !== nextDeps.length ||
      previousDeps.some((dependency, index) => !Object.is(dependency, nextDeps[index]));
    slot.effect = effect;
    slot.nextDeps = nextDeps;
    slot.pending = dependenciesChanged;
  };

  // 执行本轮已变更的业务副作用，完成定时器注册与清理。
  const flushEffects = () => {
    for (const slot of slots) {
      if (slot.kind !== "effect" || !slot.pending || !slot.effect) {
        continue;
      }
      slot.cleanup?.();
      const cleanup = slot.effect();
      slot.cleanup = typeof cleanup === "function" ? cleanup : undefined;
      slot.deps = slot.nextDeps ?? null;
      slot.pending = false;
    }
  };

  // 执行组件卸载清理，验证流式定时器不会泄漏到下一次业务渲染。
  const unmount = () => {
    for (const slot of slots) {
      slot.cleanup?.();
      slot.cleanup = undefined;
    }
  };

  // 清空测试 Hook 状态，隔离不同流式回复的业务场景。
  const reset = () => {
    unmount();
    slots.length = 0;
    cursor = 0;
  };

  return { beginRender, flushEffects, reset, unmount, useEffect, useMemo, useRef, useState };
});

vi.mock("react", async () => {
  const actual = await vi.importActual<typeof import("react")>("react");
  return {
    ...actual,
    useEffect: hookHarness.useEffect,
    useMemo: hookHarness.useMemo,
    useRef: hookHarness.useRef,
    useState: hookHarness.useState,
  };
});

vi.mock("../../workers/markdownParserCore", () => ({
  renderMarkdownToHtml: renderMarkdownToHtmlMock,
}));

vi.mock("./useChatFileContextMenu", () => ({
  useChatFileContextMenu: () => ({
    openLocalFileContextMenu: vi.fn(),
    contextMenu: null,
  }),
}));

let fakeNow = 0;

// 使用 Hook 测试运行器渲染 Markdown 组件并提交其业务副作用。
function renderMarkdownContent(props: Parameters<typeof MarkdownContent>[0]) {
  hookHarness.beginRender();
  const rendered = MarkdownContent(props);
  hookHarness.flushEffects();
  return rendered;
}

describe("MarkdownContent 流式 Markdown 解析节流", () => {
  beforeEach(() => {
    fakeNow = 0;
    hookHarness.reset();
    renderMarkdownToHtmlMock.mockClear();
    vi.useFakeTimers();
    vi.spyOn(performance, "now").mockImplementation(() => fakeNow);
  });

  afterEach(() => {
    hookHarness.unmount();
    vi.clearAllTimers();
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("流式内容在 300ms 内多次变化时只解析一次并复用旧 HTML", () => {
    renderMarkdownContent({ content: "stream-0", streaming: true });
    expect(renderMarkdownToHtmlMock).toHaveBeenCalledTimes(1);

    fakeNow = 50;
    renderMarkdownContent({ content: "stream-1", streaming: true });
    fakeNow = 100;
    renderMarkdownContent({ content: "stream-2", streaming: true });
    fakeNow = 200;
    renderMarkdownContent({ content: "stream-3", streaming: true });

    expect(renderMarkdownToHtmlMock).toHaveBeenCalledTimes(1);
    expect(vi.getTimerCount()).toBe(1);
  });

  it("流式结束后立即解析最终内容并展示完整高亮排版", () => {
    renderMarkdownContent({
      content: "```ts\nconst before = true;\n```",
      streaming: true,
    });
    fakeNow = 100;
    const rendered = renderMarkdownContent({
      content: "```ts\nconst final = true;\n```",
      streaming: false,
    }) as { props: { children: [{ props: { dangerouslySetInnerHTML: { __html: string } } }] } };
    const renderedHtml = rendered.props.children[0].props.dangerouslySetInnerHTML.__html;

    expect(renderMarkdownToHtmlMock).toHaveBeenCalledTimes(2);
    expect(renderedHtml).toContain("class=\"hljs\"");
    expect(renderedHtml).toContain("const final = true;");
    expect(vi.getTimerCount()).toBe(0);
  });

  it("组件卸载时清理尚未到期的流式解析定时器", () => {
    renderMarkdownContent({ content: "stream-timer", streaming: true });
    expect(vi.getTimerCount()).toBe(1);

    hookHarness.unmount();

    expect(vi.getTimerCount()).toBe(0);
  });
});
