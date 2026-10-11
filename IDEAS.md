# Ideas / explorations

Loose ideas for what QuestPad could do next. The protocol and register map are in PROTOCOL.md.

## Buttons
- **Choose what Oculus / Menu do**: a dropdown in the control panel per button, with a short list of what people
  actually want (Guide, Start, Back / Select, nothing) instead of every button there is.

## Power
- **Standby when left alone**: put the controllers down and after 3 to 5 minutes they drop to standby (reg 0x06),
  after 10 they switch off. Moving one wakes it again.

## Battery
- Runtime estimate from the AA voltage curve (reg 0x2f mV) logged over time.

## Making it accessible
To rethink from scratch: what does getting QuestPad running look like for someone who isn't a developer?

## Tooling
- Decode calibration registers (0x4c / 0x4f / 0x99) so stick centre / trigger ranges use factory values.

## Further out
- **Camera tracking**: an IR camera watching the controllers' IR LED ring (we control its flash timing, reg 0x28)
  would bring these controllers closer to working with other inside-out headsets. Not a priority.
- **Bluetooth**: an app on the Quest that flashes the controllers with firmware that makes them ordinary Bluetooth
  controllers. Pie in the sky; left alone for a while.
