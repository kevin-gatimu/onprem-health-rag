// Shared page-width container. Two archetypes:
//   flow  — reading/form/wizard pages (chat, ingest, profile, settings). Centered
//           with a comfortable, breakpoint-scaled cap so line lengths stay readable.
//   board — data-dense pages (dashboard, tables, card grids). Fills the available
//           width, with a ceiling on very wide (4K+) screens so it never sprawls.
import type { ReactNode } from "react";
import { cn } from "../ui/cn";

type Variant = "flow" | "board";

const VARIANTS: Record<Variant, string> = {
  flow: "mx-auto w-full max-w-5xl 3xl:max-w-6xl 4xl:max-w-7xl",
  board: "w-full 4xl:mx-auto 4xl:max-w-[2560px]",
};

export function PageContainer({
  variant = "flow",
  className,
  children,
}: {
  variant?: Variant;
  className?: string;
  children: ReactNode;
}) {
  return <div className={cn(VARIANTS[variant], className)}>{children}</div>;
}
