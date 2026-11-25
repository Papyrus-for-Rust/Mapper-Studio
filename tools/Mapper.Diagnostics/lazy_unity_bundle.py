"""Read current UnityFS bundles without expanding multi-gigabyte resource bodies.

UnityPy handles serialized object/type-tree decoding. This adapter supplies bounded,
seekable node streams and decodes only the UnityFS blocks those objects touch.
Supports uncompressed/LZ4 UnityFS v7/8; other variants fail explicitly.
"""
import bisect
from collections import OrderedDict
import io
from struct import Struct, unpack_from

import lz4.block
import UnityPy
from UnityPy.files.BundleFile import BundleFile
from UnityPy.files.SerializedFile import SerializedFile
from UnityPy.helpers import ImportHelper
from UnityPy.streams import EndianBinaryReader
from UnityPy.streams.EndianBinaryReader import EndianBinaryReader_Streamable

MAX_READ = 64 * 1024**2
MAX_METADATA = 64 * 1024**2


class MetadataReader(EndianBinaryReader_Streamable):
    """Read a bounded metadata prefix directly, keeping object payloads lazy."""
    _function_map = {"<": {}, ">": {}}

    def __new__(cls, *args):
        return object.__new__(cls)

    def __init__(self, blocks, offset, size, prefix):
        self.blocks, self.offset, self.prefix = blocks, offset, prefix
        self.stream = NodeStream(blocks, offset, size)
        EndianBinaryReader.__init__(self, b"")

    @property
    def Length(self):
        return self.stream.size

    @property
    def Position(self):
        return self.stream.position

    @Position.setter
    def Position(self, value):
        if not 0 <= value <= self.Length:
            raise ValueError("UnityFS seek outside node")
        self.stream.position = value

    def read(self, count=-1):
        position = self.stream.position
        if count is None or count < 0:
            count = self.Length - position
        count = min(count, self.Length - position)
        if count > MAX_READ:
            raise ValueError("UnityFS read outside bounded node")
        end = position + count
        result = (self.prefix[position:end] if end <= len(self.prefix)
                  else self.blocks.read_at(self.offset + position, count))
        self.stream.position = end
        return result

    @property
    def bytes(self):
        position = self.Position
        try:
            self.Position = 0
            return self.read()
        finally:
            self.Position = position


def _metadata_number(struct):
    def read(reader):
        position = reader.stream.position
        if position + struct.size <= len(reader.prefix):
            result = struct.unpack_from(reader.prefix, position)[0]
            reader.stream.position += struct.size
            return result
        return struct.unpack(reader.read(struct.size))[0]
    return read


for _endian in ("<", ">"):
    for _name, _format in (("short", "h"), ("u_short", "H"), ("int", "i"),
                           ("u_int", "I"), ("long", "q"), ("u_long", "Q"),
                           ("half", "e"), ("float", "f"), ("double", "d")):
        MetadataReader._function_map[_endian]["read_" + _name] = _metadata_number(Struct(_endian + _format))


def node_reader(blocks, node, available):
    """Only recognized serialized headers get an in-memory metadata prefix."""
    if node.size >= 20:
        header = blocks.read_at(node.offset, min(node.size, 48))
        metadata, size, version, offset = unpack_from(">IIII", header)
        if 9 <= version <= 22 and header[16] in (0, 1):
            if version >= 22:
                if len(header) < 48:
                    return EndianBinaryReader(BufferedNodeStream(NodeStream(blocks, node.offset, node.size))), 0
                metadata, size, offset = unpack_from(">Iqq", header, 20)
            minimum = 48 if version >= 22 else 20
            if size == node.size and 0 < metadata <= offset and minimum <= offset <= min(node.size, available):
                prefix = blocks.read_at(node.offset, offset)
                return MetadataReader(blocks, node.offset, node.size, prefix), offset
    return EndianBinaryReader(BufferedNodeStream(NodeStream(blocks, node.offset, node.size))), 0


def decompress(data, size, flags):
    if not 0 <= size <= MAX_READ:
        raise ValueError("UnityFS block exceeds decoding budget")
    codec = flags & 63
    if codec == 0:
        result = data
    elif codec in (2, 3):
        result = lz4.block.decompress(data, uncompressed_size=size)
    else:
        raise ValueError(f"Unsupported UnityFS compression {codec}")
    if len(result) != size:
        raise ValueError("UnityFS block length mismatch")
    return result


class Blocks:
    def __init__(self, reader, blocks, data_start, data_end):
        self.reader = reader
        self.blocks = []
        self.starts = []
        self.length = 0
        at = data_start
        for size, compressed, flags in blocks:
            stored = flags & 63 == 0
            if (not 0 < size <= (16 * 1024**3 if stored else 8 * 1024**2)
                    or not 0 < compressed or (stored and compressed != size)
                    or (not stored and compressed > MAX_READ) or at + compressed > data_end):
                raise ValueError(f"Invalid UnityFS block bounds: size={size}, compressed={compressed}, at={at}, end={data_end}")
            self.starts.append(self.length)
            self.blocks.append((at, compressed, size, flags))
            self.length += size
            at += compressed
        if self.length > 32 * 1024**3:
            raise ValueError("UnityFS expanded size exceeds budget")
        self.cache = OrderedDict()

    def read_at(self, start, count):
        if start < 0 or count < 0 or count > MAX_READ or start + count > self.length:
            raise ValueError("UnityFS read outside bounded node")
        parts = []
        while count:
            i = bisect.bisect_right(self.starts, start) - 1
            at, compressed, size, flags = self.blocks[i]
            offset = start - self.starts[i]
            take = min(count, size - offset)
            if flags & 63 == 0:
                self.reader.Position = at + offset
                piece = self.reader.read_bytes(take)
                if len(piece) != take: raise ValueError("Truncated UnityFS stored block")
                parts.append(piece)
                start += take
                count -= take
                continue
            if i not in self.cache:
                self.reader.Position = at
                self.cache[i] = decompress(self.reader.read_bytes(compressed), size, flags)
                if len(self.cache) > 2:
                    self.cache.popitem(last=False)
            self.cache.move_to_end(i)
            block = self.cache[i]
            parts.append(block[offset:offset + take])
            start += take
            count -= take
        return b"".join(parts)


class NodeStream(io.RawIOBase):
    def __init__(self, blocks, offset, size):
        super().__init__()
        if offset < 0 or size < 0 or offset + size > blocks.length:
            raise ValueError("UnityFS node exceeds expanded stream")
        self.blocks, self.offset, self.size, self.position = blocks, offset, size, 0

    def readable(self): return True
    def seekable(self): return True
    def tell(self): return self.position

    def seek(self, offset, whence=0):
        position = offset if whence == 0 else self.position + offset if whence == 1 else self.size + offset if whence == 2 else -1
        if not 0 <= position <= self.size:
            raise ValueError("UnityFS seek outside node")
        self.position = position
        return position

    def read(self, count=-1):
        if count is None or count < 0:
            count = self.size - self.position
        count = min(count, self.size - self.position)
        result = self.blocks.read_at(self.offset + self.position, count)
        self.position += len(result)
        return result

    def readinto(self, buffer):
        result = self.read(len(buffer))
        buffer[:len(result)] = result
        return len(result)


class BufferedNodeStream(io.BufferedReader):
    """Coalesce metadata reads while preserving the per-read allocation limit."""
    def __init__(self, node):
        super().__init__(node, buffer_size=65536)

    def read(self, count=-1):
        if count is None or count < 0 or count > MAX_READ:
            remaining = self.raw.size - self.tell()
            count = remaining if count is None or count < 0 else min(count, remaining)
            if count > MAX_READ:
                raise ValueError("UnityFS read outside bounded node")
        return super().read(count)


class LazyBundle(BundleFile):
    def __init__(self, *args, selected_nodes=None, follow_dependencies=False, **kwargs):
        self.selected_nodes = selected_nodes
        self.follow_dependencies = follow_dependencies
        super().__init__(*args, **kwargs)

    def read_fs(self, reader):
        if self.signature != "UnityFS" or self.version not in (7, 8):
            raise ValueError("Lazy adapter requires UnityFS v7 or v8")
        total = reader.read_long()
        compressed, expanded, flags = reader.read_u_int(), reader.read_u_int(), reader.read_u_int()
        if total != reader.Length or not 0 < compressed <= 32 * 1024**2 or not 0 < expanded <= MAX_READ:
            raise ValueError("Invalid UnityFS metadata lengths")
        if flags & ~0x2FF or not flags & 0x40:
            raise ValueError("Unsupported UnityFS flags")
        reader.align_stream(16)
        start = reader.Position
        metadata_at = total - compressed if flags & 0x80 else start
        if metadata_at < start:
            raise ValueError("Invalid UnityFS metadata offset")
        reader.Position = metadata_at
        info = EndianBinaryReader(decompress(reader.read_bytes(compressed), expanded, flags))
        info.read_bytes(16)
        count = info.read_int()
        if not 0 < count <= 262144:
            raise ValueError("UnityFS block count exceeds budget")
        blocks = [(info.read_u_int(), info.read_u_int(), info.read_u_short()) for _ in range(count)]
        count = info.read_int()
        if not 0 < count <= 8192:
            raise ValueError("UnityFS node count exceeds budget")
        from collections import namedtuple
        Node = namedtuple("Node", "offset size flags path")
        nodes = []
        seen_paths = set()
        for _ in range(count):
            offset, size, node_flags = info.read_long(), info.read_long(), info.read_u_int()
            path = info.read_string_to_null(4096)
            if not path or len(path) > 4095 or path in seen_paths:
                raise ValueError("Invalid or duplicate UnityFS node path")
            seen_paths.add(path)
            nodes.append(Node(offset, size, node_flags, path))
        data_start = start if flags & 0x80 else start + compressed
        if flags & 0x200:
            data_start = (data_start + 15) // 16 * 16
        self.blocks = Blocks(reader, blocks, data_start, metadata_at if flags & 0x80 else total)
        return nodes, self.blocks

    def read_files(self, blocks, nodes):
        if self.selected_nodes is not None:
            missing = self.selected_nodes - {node.path for node in nodes}
            if missing:
                raise ValueError("Requested UnityFS nodes are missing: " + ", ".join(sorted(missing)[:8]))
        by_name = {node.path.lower(): node for node in nodes}
        if len(by_name) != len(nodes):
            raise ValueError("Ambiguous UnityFS dependency names")
        pending = [node for node in nodes if self.selected_nodes is None or node.path in self.selected_nodes]
        scheduled = {node.path for node in pending}
        metadata_remaining = MAX_METADATA
        for node in pending:
            reader, metadata_size = node_reader(blocks, node, metadata_remaining)
            metadata_remaining -= metadata_size
            value = ImportHelper.parse_file(reader, self, node.path)
            if isinstance(value, (EndianBinaryReader, SerializedFile)) and self.environment:
                self.environment.register_cab(node.path, value)
            value.flags = node.flags
            self.files[node.path] = value
            if self.follow_dependencies and isinstance(value, SerializedFile):
                # Follow only names present in this bundle's validated index.
                # Other bundle dependencies retain the normal explicit resolver.
                for external in value.externals:
                    name = external.path.replace('\\', '/').rsplit('/', 1)[-1].lower()
                    dependency = by_name.get(name)
                    if dependency is not None and dependency.path not in scheduled:
                        scheduled.add(dependency.path)
                        pending.append(dependency)


def load(path, *, nodes=None, follow_dependencies=False):
    # Exact caller-declared dependencies only. The default still opens every
    # serialized node for clients that resolve cross-file Unity references.
    selected = None if nodes is None else frozenset(nodes)
    if selected is not None and (not 1 <= len(selected) <= 8192 or any(
            not isinstance(name, str) or not 1 <= len(name) <= 4095 for name in selected)):
        raise ValueError("Invalid UnityFS node selection")
    environment = UnityPy.Environment()
    stream = open(path, "rb")
    try:
        bundle = LazyBundle(EndianBinaryReader(stream), environment, name=str(path), selected_nodes=selected, follow_dependencies=follow_dependencies)
        environment.files[str(path)] = bundle
        environment.path = str(path.parent)
        return environment, stream
    except BaseException:
        stream.close()
        raise


if __name__ == "__main__":
    import sys
    from pathlib import Path
    environment, stream = load(Path(sys.argv[1]))
    try:
        print("Serialized nodes:", [(a.name, len(a.objects)) for a in environment.assets])
        container = environment.container
        print("Container entries:", len(container))
        for path, obj in container.items():
            if path.lower() == "assets/manifest.asset":
                fields = obj.read_typetree()
                print("Manifest fields:", [(k, len(v) if isinstance(v, (list, dict)) else type(v).__name__) for k,v in fields.items()])
                for item in fields.get("guidPaths", []):
                    if item.get("guid") == (sys.argv[2] if len(sys.argv)>2 else "5510809b33cbc465c9cf012140cf5713"): print("Requested GUID:", item)
    finally:
        stream.close()
