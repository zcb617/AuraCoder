import type { CSSProperties, SVGProps } from "react";
// 旧版深色 lockup SVG 保留备查，不再由当前品牌组件渲染。
// import lockupOnDark from "../../assets/brand/auracoder-lockup-on-dark.svg";
// 旧版浅色 lockup SVG 保留备查，不再由当前品牌组件渲染。
// import lockupOnLight from "../../assets/brand/auracoder-lockup-on-light.svg";

interface AuraCoderMarkProps extends Omit<SVGProps<SVGSVGElement>, "width" | "height"> {
  size?: number;
  title?: string;
  accent?: string;
}

export function AuraCoderMark({
  size = 20,
  title,
  accent = "var(--accent)",
  style,
  ...props
}: AuraCoderMarkProps) {
  const mergedStyle: CSSProperties = {
    color: "var(--text-1)",
    flexShrink: 0,
    ...style,
  };

  return (
    <svg
      viewBox="0 0 64 64"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      width={size}
      height={size}
      role={title ? "img" : undefined}
      aria-label={title}
      aria-hidden={title ? undefined : true}
      style={mergedStyle}
      {...props}
    >
      <rect
        x="8"
        y="8"
        width="48"
        height="48"
        rx="12"
        stroke="currentColor"
        strokeWidth="4"
      />
      <path
        d="M26 10V54M28 27H54"
        stroke="currentColor"
        strokeWidth="4"
        strokeLinecap="round"
      />
      <rect x="34" y="34" width="14" height="14" rx="5" fill={accent} />
    </svg>
  );
}

// 渲染带 AuraCoderMark 图形和 AuraCoder 文字的完整产品品牌锁定组合。
export function AuraCoderLockup({ width = 136, title = "AuraCoder" }: { width?: number; title?: string }) {
  const height = width * (64 / 276);

  return (
    <span
      className="auracoder-brand-lockup"
      style={{ width, height }}
      role="img"
      aria-label={title}
    >
      <AuraCoderMark size={height} />
      <span className="auracoder-brand-text" style={{ fontSize: height * 0.55 }}>
        AuraCoder
      </span>
      {/*
      旧版 lockup 图片 JSX 保留备查，不再参与当前品牌展示：
      <img className="auracoder-brand-lockup-dark" src={lockupOnDark} alt="" />
      <img className="auracoder-brand-lockup-light" src={lockupOnLight} alt="" />
      */}
    </span>
  );
}

// 渲染用于设置页等紧凑场景的 AuraCoder 产品文字标识。
export function AuraCoderWordmark({ width = 91, title = "AuraCoder" }: { width?: number; title?: string }) {
  // 旧版 lockup 裁切计算保留备查，不再参与当前品牌展示：
  // const fullLockupWidth = width * (276 / 182);
  // const wordmarkOffset = width * (84 / 182);
  const height = width * (64 / 182);

  return (
    <span
      className="auracoder-brand-wordmark"
      style={{ width, height }}
      role="img"
      aria-label={title}
    >
      <span className="auracoder-brand-text" style={{ fontSize: width / 5.4 }}>
        AuraCoder
      </span>
      {/*
      旧版 wordmark 图片 JSX 与裁切结果保留备查，不再参与当前品牌展示：
      <img
        className="auracoder-brand-lockup-dark"
        src={lockupOnDark}
        alt=""
        style={{ width: fullLockupWidth, transform: `translateX(-${wordmarkOffset}px)` }}
      />
      <img
        className="auracoder-brand-lockup-light"
        src={lockupOnLight}
        alt=""
        style={{ width: fullLockupWidth, transform: `translateX(-${wordmarkOffset}px)` }}
      />
      */}
    </span>
  );
}
