import { useState } from 'preact/hooks';
import type { JSX } from 'preact';
import { postConfig } from '../lib/api';
import { fmtRange } from '../lib/format';
import { toast, errMsg } from '../lib/toast';
import type { CoolingBounds, CoolingConfig, Range, ThermalBounds, ThermalConfig } from '../lib/types';
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
  const [dangerC, setDangerC] = useState<string | number>(() => (thermal ? thermal.dangerous_temp_c : ''));
  const [chipC, setChipC] = useState<string | number>(() => (thermal ? thermal.dangerous_chip_temp_c : ''));
  const [cooldownC, setCooldownC] = useState<string | number>(() => (thermal ? thermal.cooldown_temp_c : ''));
  const [targetC, setTargetC] = useState<string | number>(() =>
    cooling && cooling.target_temp_c != null ? cooling.target_temp_c : '',
  );
  const [minFanPwm, setMinFanPwm] = useState<string | number>(() =>
    cooling && cooling.min_fan_pwm != null ? cooling.min_fan_pwm : '',
  );

  const danger: Partial<Range> = thermalBounds.dangerousTempC || {};
  const chip: Partial<Range> = thermalBounds.dangerousChipTempC || {};
  const cooldown: Partial<Range> = thermalBounds.cooldownTempC || {};
  const target: Partial<Range> = coolingBounds.targetTempC || {};

  async function save(event: JSX.TargetedEvent<HTMLFormElement, SubmitEvent>): Promise<void> {
    event.preventDefault();
    try {
      await postConfig({
        thermal: {
          sample_interval_ms: sampleMs,
          dangerous_temp_c: +dangerC,
          cooldown_temp_c: +cooldownC,
          dangerous_chip_temp_c: +chipC,
        },
        cooling: { target_temp_c: +targetC, min_fan_pwm: +minFanPwm },
      });
      toast.ok('Changes saved');
    } catch (error) {
      toast.err('Save failed', errMsg(error));
    }
  }

  return (
    <>
    <h2 class="cfgTitle">Temperature</h2>
    <form onSubmit={save}>
      <div class="fldRows">
          <Field
            label="Danger threshold"
            desc={fmtRange(danger.start, danger.end)}
            unit="°C"
            value={dangerC}
            onInput={(e) => setDangerC(e.currentTarget.value)}
            type="number"
            step="0.5"
            min={danger.start}
            max={danger.end}
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
          <Field
            label="Target temperature"
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
            label="Min fan"
            desc={fmtRange(0, 100)}
            unit="%"
            value={minFanPwm}
            onInput={(e) => setMinFanPwm(e.currentTarget.value)}
            type="number"
            step="1"
            min="0"
            max="100"
          />
      </Section>
      <button type="submit">Save</button>
    </form>
    </>
  );
}
