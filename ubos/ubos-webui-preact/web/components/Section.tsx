import type { ComponentChildren } from 'preact';

export interface SectionProps {
  title: ComponentChildren;
  children?: ComponentChildren;
  sep?: boolean;
}

export default function Section({ title, children, sep }: SectionProps) {
  return (
    <div class={'psec' + (sep ? ' sep' : '')}>
      <h3>{title}</h3>
      <div class="fldRows">{children}</div>
    </div>
  );
}
