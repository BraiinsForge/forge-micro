import { useState } from 'preact/hooks';
import type { JSX } from 'preact';
import { saveConfig } from '../lib/api';
import { fmtRange } from '../lib/format';
import { toast } from '../lib/toast';
import type { CoolingBounds, CoolingConfig, FanMode, Range, ThermalBounds, ThermalConfig } from '../lib/types';
import Field from './Field';
import Section from './Section';

export interface ConfigThermalProps {
  thermal?: ThermalConfig;
  cooling?: CoolingConfig;
  thermalBounds: Partial<ThermalBounds>;
  coolingBounds: Partial<CoolingBounds>;
}

export default function ConfigThermal({ thermal, cooling, thermalBounds, coolingBounds }: ConfigThermalProps) {
  const [sampleMs] = useState<number | null>(() => (thermal ? thermal.sample_interval_ms : null));
  const [hashboardC, setHashboardC] = useState<string | number>(() =>
    thermal ? thermal.dangerous_hashboard_temp_c : '',
  );
  const [chipC, setChipC] = useState<string | number>(() => (thermal ? thermal.dangerous_chip_temp_c : ''));
  const [cooldownC, setCooldownC] = useState<string | number>(() => (thermal ? thermal.cooldown_temp_c : ''));
  const [mode, setMode] = useState<FanMode>(() => (cooling && cooling.mode != null ? cooling.mode : 'auto'));
  const [targetC, setTargetC] = useState<string | number>(() =>
    cooling && cooling.target_temp_c != null ? cooling.target_temp_c : '',
  );
  const [minFanPwm, setMinFanPwm] = useState<string | number>(() =>
    cooling && cooling.min_fan_pwm != null ? cooling.min_fan_pwm : '',
  );
  const [fixedFanPwm, setFixedFanPwm] = useState<string | number>(() =>
    cooling && cooling.fixed_fan_pwm != null ? cooling.fixed_fan_pwm : '',
  );

  const hashboard: Partial<Range> = thermalBounds.dangerousHashboardTempC || {};
  const chip: Partial<Range> = thermalBounds.dangerousChipTempC || {};
  const cooldown: Partial<Range> = thermalBounds.cooldownTempC || {};
  const target: Partial<Range> = coolingBounds.targetTempC || {};
  const floor: Partial<Range> = coolingBounds.minFanPwm || {};
  const fixed: Partial<Range> = coolingBounds.fixedFanPwm || {};
  const isFixed = mode === 'fixed';

  // The floor bounds PID output and the target is its setpoint, so both
  // are dead under fixed, and the fixed duty is dead under auto. All
  // three stay visible, enabled, and keep their value across a toggle,
  // and the dead ones are dimmed so the eye lands on the live ones:
  // the mode control says what governs the fan, the layout never
  // moves, and every field stays under the browser's range check, which
  // a disabled control is barred from. The save carries all six, so an
  // empty duty is refused here by name: an emptied number input coerces
  // to 0, which is in range for a duty, so the board cannot refuse it
  // the way it refuses the fields whose range starts above zero.
  const save = (event: JSX.TargetedEvent<HTMLFormElement, SubmitEvent>) => {
    event.preventDefault();
    if (minFanPwm === '') return toast.err('Min fan speed must be set.');
    if (fixedFanPwm === '') return toast.err('Fixed fan speed must be set.');
    return saveConfig({
      thermal: {
        sample_interval_ms: sampleMs,
        dangerous_hashboard_temp_c: +hashboardC,
        cooldown_temp_c: +cooldownC,
        dangerous_chip_temp_c: +chipC,
      },
      cooling: {
        mode,
        target_temp_c: +targetC,
        fixed_fan_pwm: +fixedFanPwm,
        min_fan_pwm: +minFanPwm,
      },
    });
  };

  return (
    <>
    <h2 class="cfgTitle">Temperature</h2>
    <form onSubmit={save}>
      <div class="fldRows">
          <Field
            label="Hashboard danger threshold"
            desc={fmtRange(hashboard.start, hashboard.end)}
            unit="°C"
            value={hashboardC}
            onInput={(e) => setHashboardC(e.currentTarget.value)}
            type="number"
            step="0.5"
            min={hashboard.start}
            max={hashboard.end}
          />
          <Field
            label="Chip danger threshold"
            desc={fmtRange(chip.start, chip.end)}
            unit="°C"
            value={chipC}
            onInput={(e) => setChipC(e.currentTarget.value)}
            type="number"
            step="0.5"
            min={chip.start}
            max={chip.end}
          />
          <Field
            label="Cooldown threshold"
            desc={fmtRange(cooldown.start, cooldown.end)}
            unit="°C"
            value={cooldownC}
            onInput={(e) => setCooldownC(e.currentTarget.value)}
            type="number"
            step="0.5"
            min={cooldown.start}
            max={cooldown.end}
          />
      </div>
      <Section title="Cooling" sep>
          <div class="fld">
            <span class="fldText">
              <span class="fldLabel">Fan mode</span>
            </span>
            <span class="fldCtl">
              <div class="seg">
                <button type="button" class={!isFixed ? 'active' : ''} onClick={() => setMode('auto')}>
                  Auto
                </button>
                <button type="button" class={isFixed ? 'active' : ''} onClick={() => setMode('fixed')}>
                  Fixed
                </button>
              </div>
            </span>
          </div>
          <Field
            label="Target temperature"
            off={isFixed}
            desc={fmtRange(target.start, target.end)}
            unit="°C"
            value={targetC}
            onInput={(e) => setTargetC(e.currentTarget.value)}
            type="number"
            step="0.5"
            min={target.start}
            max={target.end}
          />
          <Field
            label="Min fan speed"
            off={isFixed}
            desc={fmtRange(floor.start, floor.end)}
            unit="%"
            value={minFanPwm}
            onInput={(e) => setMinFanPwm(e.currentTarget.value)}
            type="number"
            step="1"
            min={floor.start}
            max={floor.end}
          />
          <Field
            label="Fixed fan speed"
            off={!isFixed}
            desc={fmtRange(fixed.start, fixed.end)}
            unit="%"
            value={fixedFanPwm}
            onInput={(e) => setFixedFanPwm(e.currentTarget.value)}
            type="number"
            step="1"
            min={fixed.start}
            max={fixed.end}
          />
      </Section>
      <button type="submit">Save</button>
    </form>
    </>
  );
}
