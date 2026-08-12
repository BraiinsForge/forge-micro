import type { ComponentChildren, JSX } from 'preact';

export interface SelectOption {
  value: string | number;
  label: ComponentChildren;
}

export interface SelectProps {
  label?: ComponentChildren;
  desc?: ComponentChildren;
  value?: string | number;
  onInput?: JSX.GenericEventHandler<HTMLSelectElement>;
  options: SelectOption[];
  disabled?: boolean;
  class?: string;
}

export default function Select({ label, desc, value, onInput, options, disabled, class: extra }: SelectProps) {
  const ctl = (
    <span class="selCtl">
      <select value={value} onInput={onInput} disabled={disabled}>
        {options.map((o) => (
          <option value={o.value} key={o.value}>
            {o.label}
          </option>
        ))}
      </select>
      <svg viewBox="0 0 16 16" aria-hidden="true">
        <path d="M8 11 3 6 3.7 5.3 8 9.6 12.3 5.3 13 6z" />
      </svg>
    </span>
  );

  if (!label) return ctl;

  return (
    <label class={'fld' + (extra ? ' ' + extra : '')}>
      <span class="fldText">
        <span class="fldLabel">{label}</span>
        {desc ? <span class="fldDesc">{desc}</span> : null}
      </span>
      {ctl}
    </label>
  );
}
