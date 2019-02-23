#!/usr/bin/env python3
# -*- coding: utf-8 -*-

import sys
import numpy as np
import matplotlib.pyplot as plt
import matplotlib.animation as animation
import re
import struct

# The update graph function
def animate(data):
    global x_max, x_min, y_max, y_min, x, y
    try:
        # we must recalculate the abscissa range
        x_new = float(data[0])
        y_new = float(data[1])

        # Autoscale
        if x_new > x_max:
            x_max = x_new
        if x_new < x_min:
            x_min = x_new
        if y_new > y_max:
            y_max = y_new
        if y_new < y_min:
            y_min = y_new
        ax.set_xlim(x_min * 0.99 - 1, x_max * 1.01 + 1)
        ax.set_ylim(y_min * 0.99 - 1, y_max * 1.01 + 1)

        # add the new plot coordinate
        x.append(x_new)
        y.append(y_new)
        line.set_data(x, y)

        return line,

    except KeyboardInterrupt:
        sys.exit(0)

# The data generator take its
# input from file or stdin
def data_gen():
    while True:
        line = fd.read(8)
        if len(line) == 8:
            value = struct.unpack('<II', line)
            print(value)
            yield value

if __name__ == '__main__':
    fd = sys.stdin.buffer

    fig = plt.figure()
    ax  = fig.add_subplot(111)

    line, = ax.plot([], [], 'ro') 
    x_min = 999999999
    x_max = 0
    y_min = 999999999
    y_max = 0
    x = []
    y = []

    anim = animation.FuncAnimation(fig, animate, frames=data_gen, repeat=False)

    plt.show()