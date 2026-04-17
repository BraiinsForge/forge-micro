# DevEBox STM32F407 + WT32-ETH01 (cannibalized)

The LAN8720A PHY comes from a WT32-ETH01 board with the ESP32 module
removed. Wires soldered directly to the exposed ESP32 pads connect
the RMII signals to the DevEBox STM32F407VGT6 headers.

## Wiring

### RMII data signals (ESP32 pads to DevEBox headers)

| Signal  | WT32-ETH01 pad | STM32F407 pin |
|---------|----------------|---------------|
| TXD0    | GPIO19         | PB12          |
| TXD1    | GPIO22         | PB13          |
| TX_EN   | GPIO21         | PB11          |
| RXD0    | GPIO25         | PC4           |
| RXD1    | GPIO26         | PC5           |
| CRS_DV  | GPIO27         | PA7           |
| REF_CLK | GPIO0          | PA1           |

### Management interface

| Signal | WT32-ETH01 pad | STM32F407 pin |
|--------|----------------|---------------|
| MDC    | GPIO23         | PC1           |
| MDIO   | GPIO18         | PA2           |

### Oscillator enable

| Signal     | WT32-ETH01 pad | STM32F407 pin |
|------------|----------------|---------------|
| OSC enable | GPIO16         | PE14          |

PE14 is driven high by firmware to enable the 50 MHz crystal oscillator
on the WT32-ETH01 board. Without it the LAN8720A has no reference clock.

### Power

| Signal | WT32-ETH01 header | DevEBox |
|--------|-------------------|---------|
| 3.3V   | 3V3               | 3.3V    |
| GND    | GND               | GND     |

## Notes

- PHY address is 1 (set by pull-up on PHYAD0).
- PHY reset is handled by an RC circuit on the WT32-ETH01 (10k x 0.1uF).
  No GPIO control over nRST.
- RX_ER (GPIO13) is not connected. RMII signals receive errors through
  CRS_DV instead.

## Debug probe

ST-Link V3 connected via SWD header (SWDIO, SWCLK, GND).
