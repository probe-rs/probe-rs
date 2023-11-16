* Mark all ARM memory accesses as cacheable, to indicate they must not bypass
  the cache and instead should see the same data as the CPU. Fixes #1715.
