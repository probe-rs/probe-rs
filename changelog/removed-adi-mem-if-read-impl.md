`read` method of ADI memory interface could read outside of the address range for unaligned
start addresses/blocks. Removed so that default trait implemenation is used instead.
