# Scenes

## Shipped

**Kabukichō Gate** (`kabukicho-gate`). The red gate of Kabukichō Ichiban-gai at night, rain falling in six depth-tested sheets in front of and behind it, mist drifting down the street, the odd drop on the lens. Typing makes the neon stutter, a busy CPU makes the rain heavy, a failed build strobes the gate red and a passing one sweeps it green, billboards pulse to the music, the street cools to blue at dusk and goes dark and misty at 3 am with half the shops closed, and the lock stops it all. Photo: Basile Morin, CC BY-SA 4.0.

## Next (not built yet)

Four more Shinjuku scenes, each built the same way (a photo, a depth map, masks, one shader) and each with its own idea of what the signals mean, so they don't all feel like the gate with a different picture. Source photos are the ones KubaOS already credits (Unsplash); each would get its own depth and masks from `tools/`.

### Yodobashi blue rain

Nishi-Shinjuku in a downpour, Yodobashi Camera's red over a blue-lit street that's all reflection.

- **Rain** is the subject: heavier by default, with a wet street that mirrors the signs (a screen-space reflection of the image flipped about the kerb line, broken up by ripple normals). CPU load turns drizzle into a downpour and the ripples multiply.
- **Typing** makes the puddles shiver: each burst sends rings across the reflections.
- **Builds:** failed turns the big Yodobashi sign's red into a slow alarm beat; passed washes the street reflection in a cool green that fades.
- **Music** pulses the blue street lights, not the signs, so the reflection breathes with the bass.
- **Time:** at dusk the sky is still light behind the towers; at 3 am the blue lights go out one block at a time and only the rain and the red sign are left.
- **Cursor:** parallax between the near umbrellas, the street and the towers.

### Nishi-Shinjuku teal avenue

A wide teal avenue between office towers, rain coming in, almost no one around.

- **The camera** matters most here: the avenue has real depth, so cursor parallax is strong and workspace switches dolly the camera slowly down the avenue (workspace 1 at the near end, 9 far down it).
- **CPU load** brings the rain in from the far end of the avenue: you see it advance towards the camera.
- **Typing** lights office windows: each burst turns on a few random windows in the towers, which go dark again over a minute. A long writing session fills the towers.
- **Builds:** failed blinks the aircraft warning lights on the rooftops red; passed runs a green light up the tallest tower.
- **Music:** traffic lights and car light trails along the avenue on the beat.
- **Time:** dusk is the hero here (the teal sky against lit towers); at 3 am only the warning lights blink.

### Billboards over the tracks

Giant billboards over the Chūō line tracks, the kind with a screen the size of a building.

- **Music** owns this one: the big screens become a visualiser (bass to brightness, mids to colour shifts, beats to a cut between two looks), scanlines and moiré included.
- **Trains:** a train crosses the bottom of the frame every few minutes, faster and more often while the CPU is busy; its lit windows sweep reflections across the wet ballast.
- **Typing** glitches the screens: row tearing and colour channel slips, settling when you stop.
- **Builds:** failed puts a red "NG" frame on the biggest screen for a second; passed a green "OK".
- **Time:** after midnight the screens switch to a slow ambient loop and dim; the trains stop at 1 am.
- **Cursor:** parallax between the tracks' overhead wires, the billboards and the sky.

### Station mirror

Shinjuku Station's south side, the floor so polished it's a black mirror.

- **The mirror** is the effect: the floor reflects the station, and everything that moves above it (people's silhouettes passing as a soft blur, light flicker) moves in the reflection too.
- **Typing** sends footsteps across the floor: ripples of light in the reflection, as if someone walked through a thin film of water.
- **CPU load:** the departure boards cycle faster and more people pass.
- **Builds:** failed flips the departure board to a red "運休" (service suspended) for a moment; passed to a green "定刻" (on time).
- **Music:** the overhead lights pulse gently in time, mirrored.
- **Time:** the station empties after midnight; at 3 am it's the mirror, two lit boards and a cleaning cart's light crossing slowly.
- **Lock:** the reflection goes perfectly still, like ice. The best lock frame of the five.
