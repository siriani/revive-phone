import sys, time, evdev
from evdev import ecodes as e
d = next(evdev.InputDevice(p) for p in evdev.list_devices()
         if "Synaptics" in evdev.InputDevice(p).name)
x, y = int(sys.argv[1]), int(sys.argv[2])
for t, c, v in [(e.EV_ABS, e.ABS_MT_TRACKING_ID, 7), (e.EV_ABS, e.ABS_MT_POSITION_X, x),
                (e.EV_ABS, e.ABS_MT_POSITION_Y, y), (e.EV_KEY, e.BTN_TOUCH, 1), (e.EV_SYN, 0, 0)]:
    d.write(t, c, v)
time.sleep(0.08)
for t, c, v in [(e.EV_ABS, e.ABS_MT_TRACKING_ID, -1), (e.EV_KEY, e.BTN_TOUCH, 0), (e.EV_SYN, 0, 0)]:
    d.write(t, c, v)
