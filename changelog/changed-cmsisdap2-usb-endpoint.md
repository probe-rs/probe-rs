
The write_bulk_endpoint/read_bulk_endpoint functions are now trait implementations on Endpoint.
They were renamed as well to write_bulk/read_bulk as the traits already imply they are
implemented and operate on Endpoint.
