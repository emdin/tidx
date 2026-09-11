CREATE OR REPLACE FUNCTION abi_uint(input BYTEA) RETURNS NUMERIC AS $$
DECLARE n NUMERIC := 0;
BEGIN
  FOR i IN 1..length(input) LOOP
    n := n * 256 + get_byte(input, i - 1);
  END LOOP;
  RETURN n;
END;
$$ LANGUAGE plpgsql IMMUTABLE STRICT;

CREATE OR REPLACE FUNCTION abi_int(input BYTEA) RETURNS NUMERIC AS $$
DECLARE
  n NUMERIC := 0;
  is_negative BOOLEAN;
BEGIN
  is_negative := get_byte(input, 0) >= 128;
  FOR i IN 1..length(input) LOOP
    n := n * 256 + get_byte(input, i - 1);
  END LOOP;
  IF is_negative THEN
    n := n - power(2::NUMERIC, length(input) * 8);
  END IF;
  RETURN n;
END;
$$ LANGUAGE plpgsql IMMUTABLE STRICT;

CREATE OR REPLACE FUNCTION abi_address(input BYTEA) RETURNS BYTEA AS $$
BEGIN
  RETURN substring(input FROM 13 FOR 20);
END;
$$ LANGUAGE plpgsql IMMUTABLE STRICT;

CREATE OR REPLACE FUNCTION abi_bool(input BYTEA) RETURNS BOOLEAN AS $$
BEGIN
  RETURN get_byte(input, length(input) - 1) != 0;
END;
$$ LANGUAGE plpgsql IMMUTABLE STRICT;

CREATE OR REPLACE FUNCTION abi_bytes(input BYTEA, offset_bytes INT DEFAULT 0) RETURNS BYTEA AS $$
DECLARE 
  data_offset INT;
  data_length INT;
BEGIN
  data_offset := abi_uint(substring(input FROM offset_bytes + 1 FOR 32))::INT;
  data_length := abi_uint(substring(input FROM data_offset + 1 FOR 32))::INT;
  RETURN substring(input FROM data_offset + 33 FOR data_length);
END;
$$ LANGUAGE plpgsql IMMUTABLE STRICT;

CREATE OR REPLACE FUNCTION abi_string(input BYTEA, offset_bytes INT DEFAULT 0) RETURNS TEXT AS $$
BEGIN
  RETURN convert_from(abi_bytes(input, offset_bytes), 'UTF8');
END;
$$ LANGUAGE plpgsql IMMUTABLE STRICT;

CREATE OR REPLACE FUNCTION format_address(input BYTEA) RETURNS TEXT AS $$
BEGIN
  RETURN '0x' || encode(input, 'hex');
END;
$$ LANGUAGE plpgsql IMMUTABLE STRICT;

CREATE OR REPLACE FUNCTION format_uint(input BYTEA) RETURNS TEXT AS $$
BEGIN
  RETURN abi_uint(input)::TEXT;
END;
$$ LANGUAGE plpgsql IMMUTABLE STRICT;

-- Word-indexed ABI decode. Event `data` is a sequence of 32-byte words; the
-- 1-arg abi_uint/format_uint read the WHOLE buffer as one integer, which is
-- correct only for single-word payloads (e.g. ERC-20 Transfer). For anything
-- wider (a 64-byte AttesterRegistered, etc.) select the word explicitly.
-- `word` is 0-based. Out-of-range words yield an empty substring -> 0.
CREATE OR REPLACE FUNCTION abi_uint(input BYTEA, word INT) RETURNS NUMERIC AS $$
  SELECT abi_uint(substring(input FROM word * 32 + 1 FOR 32));
$$ LANGUAGE sql IMMUTABLE STRICT;

-- Prefer format_uint over abi_uint when returning values through the JSON API:
-- abi_uint yields NUMERIC, which the response serializer renders via a 96-bit
-- decimal and NULLs for any value >= 2^96 (~7.9e28). format_uint returns TEXT
-- and round-trips full uint256 (2^256-1, 78 digits) intact.
CREATE OR REPLACE FUNCTION format_uint(input BYTEA, word INT) RETURNS TEXT AS $$
  SELECT abi_uint(substring(input FROM word * 32 + 1 FOR 32))::TEXT;
$$ LANGUAGE sql IMMUTABLE STRICT;

-- Build the 32-byte, left-padded, lowercase topic form of an EVM address so
-- callers can filter indexed address topics (topic1/2/3) without hand-padding.
-- Accepts checksummed (mixed-case) input, with or without the 0x prefix.
-- Invalid input raises (loud) rather than silently returning a zero-match value.
CREATE OR REPLACE FUNCTION topic_addr(addr TEXT) RETURNS BYTEA AS $$
  SELECT decode(lpad(lower(regexp_replace(addr, '^0[xX]', '')), 64, '0'), 'hex');
$$ LANGUAGE sql IMMUTABLE STRICT;
