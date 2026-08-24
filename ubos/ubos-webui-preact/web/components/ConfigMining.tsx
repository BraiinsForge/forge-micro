import { useState } from 'preact/hooks';
import type { JSX } from 'preact';
import { postConfig } from '../lib/api';
import { fmtRange } from '../lib/format';
import { toast, errMsg } from '../lib/toast';
import type { AsicBounds, Range, TuningConfig } from '../lib/types';
import Field from './Field';


export interface ConfigMiningProps {
  tuning?: TuningConfig;
  bounds: Partial<AsicBounds>;
}

export default function ConfigMining({ tuning, bounds }: ConfigMiningProps) {
  const [freqMhz, setFreqMhz] = useState<string | number>(() => (tuning ? tuning.frequency_mhz : ''));
  const [voltMv, setVoltMv] = useState<string | number>(() => (tuning ? tuning.voltage_mv : ''));

  const freq: Partial<Range> = bounds.frequencyMhz || {};
  const volt: Partial<Range> = bounds.voltageMv || {};

  async function save(event: JSX.TargetedEvent<HTMLFormElement, SubmitEvent>): Promise<void> {
    event.preventDefault();
    try {
      await postConfig({ tuning: { frequency_mhz: +freqMhz, voltage_mv: +voltMv } });
      toast.ok('Changes saved');
    } catch (error) {
      toast.err('Save failed', errMsg(error));
    }
  }

  return (
    <>
    <h2 class="cfgTitle">Mining</h2>
    <form onSubmit={save}>
      <div class="fldRows">
          <Field
            label="Frequency"
            desc={fmtRange(freq.start, freq.end)}
            unit="MHz"
            value={freqMhz}
            onInput={(e) => setFreqMhz(e.currentTarget.value)}
            type="number"
            step="1"
            min={freq.start}
            max={freq.end}
          />
          <Field
            label="Voltage"
            desc={fmtRange(volt.start, volt.end)}
            unit="mV"
            value={voltMv}
            onInput={(e) => setVoltMv(e.currentTarget.value)}
            type="number"
            step="1"
            min={volt.start}
            max={volt.end}
          />
      </div>
      <button type="submit">Save</button>
    </form>
    </>
  );
}
