import type { ComponentChildren, JSX } from 'preact';

export interface CheckboxProps {
  label?: ComponentChildren;
  checked?: boolean;
  onInput?: JSX.GenericEventHandler<HTMLInputElement>;
  disabled?: boolean;
  class?: string;
}

export default function Checkbox({ label, checked, onInput, disabled, class: extra }: CheckboxProps) {
  return (
    <label class={'chk' + (extra ? ' ' + extra : '')}>
      <input type="checkbox" checked={checked} onInput={onInput} disabled={disabled} />
      <span class="chkBox" aria-hidden="true"></span>
      <span class="chkText">{label}</span>
    </label>
  );
}
