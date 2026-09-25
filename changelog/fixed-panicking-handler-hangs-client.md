A request handler of `probe-rs serve` that ends in a panic now closes the connection. Previously, the client reported no error and waited for the reply forever.
