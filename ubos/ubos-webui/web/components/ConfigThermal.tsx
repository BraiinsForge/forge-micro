import { useState } from 'preact/hooks';
import type { JSX } from 'preact';
import { saveConfig } from '../lib/api';
import { fmtRange } from '../lib/format';
import { toast } from '../lib/toast';
import { MC_PER_C, SOURCE_LABEL, SOURCES } from '../lib/types';
import type {
  CoolingBounds,
  CoolingConfig,
  FanMode,
  Range,
  Statistic,
  ThermalBounds,
  ThermalConfig,
} from '../lib/types';
import Field from './Field';
import Section from './Section';

const STATISTICS: [Statistic, string][] = [
  ['max', 'Max'],
  ['mean', 'Mean'],
];

export interface ConfigThermalProps {
  thermal?: ThermalConfig;
  cooling?: CoolingConfig;
  thermalBounds: Partial<ThermalBounds>;
  coolingBounds: Partial<CoolingBounds>;
}

export default function ConfigThermal({ thermal, cooling, thermalBounds, coolingBounds }: ConfigThermalProps) {
  const [sampleMs] = useState<number | null>(() => (thermal ? thermal.sample_interval_ms : null));
  // A machine has a threshold only for a kind of reading it measures, and
  // the field of a kind it does not read is null and not shown.
  const hasHashboard = thermal?.dangerous_hashboard_temp !== null;
  const hasChip = thermal?.dangerous_chip_temp !== null;
  const [hashboardC, setHashboardC] = useState<string | number>(() =>
    thermal?.dangerous_hashboard_temp != null ? thermal.dangerous_hashboard_temp / MC_PER_C : '',
  );
  const [chipC, setChipC] = useState<string | number>(() =>
    thermal?.dangerous_chip_temp != null ? thermal.dangerous_chip_temp / MC_PER_C : '',
  );
  const [cooldownC, setCooldownC] = useState<string | number>(() => (thermal ? thermal.cooldown_temp / MC_PER_C : ''));
  const [mode, setMode] = useState<FanMode>(() => (cooling && cooling.mode != null ? cooling.mode : 'auto'));
  const [targetC, setTargetC] = useState<string | number>(() =>
    cooling?.target != null ? cooling.target.temp / MC_PER_C : '',
  );
  // The kind and the statistic of the target. Only a machine that reads
  // both kinds offers the kind, and the target's kind is the only reading
  // the PID follows.
  const [input, setInput] = useState(cooling?.target);
  const [minFanPwm, setMinFanPwm] = useState<string | number>(() =>
    cooling && cooling.min_fan_pwm != null ? cooling.min_fan_pwm : '',
  );
  const [fixedFanPwm, setFixedFanPwm] = useState<string | number>(() =>
    cooling && cooling.fixed_fan_pwm != null ? cooling.fixed_fan_pwm : '',
  );

  // The bounds arrive in millicelsius and the fields show degrees.
  const celsius = (range?: Range): Partial<Range> =>
    range ? { start: range.start / MC_PER_C, end: range.end / MC_PER_C } : {};
  const hashboard = celsius(thermalBounds.dangerousHashboardTemp);
  const chip = celsius(thermalBounds.dangerousChipTemp);
  const cooldown = celsius(thermalBounds.cooldownTemp);
  const target = celsius(coolingBounds.targetTemp);
  const floor: Partial<Range> = coolingBounds.minFanPwm || {};
  const fixed: Partial<Range> = coolingBounds.fixedFanPwm || {};
  const isFixed = mode === 'fixed';

  // The floor bounds PID output and the input and the target are what it
  // follows, so all are dead under fixed, and the fixed duty is dead under
  // auto. All of them stay visible, enabled, and keep their value across
  // a toggle, and the dead ones are dimmed so the eye lands on the live ones:
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
        dangerous_hashboard_temp: hasHashboard ? Math.round(+hashboardC * MC_PER_C) : null,
        cooldown_temp: Math.round(+cooldownC * MC_PER_C),
        dangerous_chip_temp: hasChip ? Math.round(+chipC * MC_PER_C) : null,
      },
      cooling: {
        mode,
        target: input && { ...input, temp: Math.round(+targetC * MC_PER_C) },
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
          {hasHashboard && (
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
          )}
          {hasChip && (
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
          )}
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
          {input && hasHashboard && hasChip && (
            <div class={'fld' + (isFixed ? ' off' : '')}>
              <span class="fldText">
                <span class="fldLabel">PID follows</span>
              </span>
              <span class="fldCtl">
                <div class="seg">
                  {SOURCES.map((source) => (
                    <button
                      type="button"
                      class={input.source === source ? 'active' : ''}
                      onClick={() => setInput({ ...input, source })}
                    >
                      {SOURCE_LABEL[source]}
                    </button>
                  ))}
                </div>
              </span>
            </div>
          )}
          <div class={'fld' + (isFixed ? ' off' : '')}>
            <span class="fldText">
              <span class="fldLabel">Target temperature</span>
              <span class="fldDesc">{fmtRange(target.start, target.end)}</span>
            </span>
            <span class="fldCtl hasUnit">
              <input
                type="number"
                value={targetC}
                onInput={(e) => setTargetC(e.currentTarget.value)}
                step="0.5"
                min={target.start}
                max={target.end}
              />
              <span class="fldUnit">°C</span>
            </span>
            {input && (
              <div class="seg">
                {STATISTICS.map(([statistic, label]) => (
                  <button
                    type="button"
                    class={input.statistic === statistic ? 'active' : ''}
                    onClick={() => setInput({ ...input, statistic })}
                  >
                    {label}
                  </button>
                ))}
              </div>
            )}
          </div>
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
