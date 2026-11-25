"""Bulk-read Unity v22 object records while retaining UnityPy's object readers.

The fixed record encoding and returned fields match UnityPy 1.25.3. Other
versions and readers use its original decoder. This changes transport only.
"""
from bisect import bisect_right
import gc
from io import BufferedReader
from itertools import islice
from struct import Struct

import lazy_unity_bundle
import UnityPy
from UnityPy.enums import ClassIDType
from UnityPy.files.ObjectReader import ObjectReader
from UnityPy.helpers.TypeTreeHelper import read_typetree_boost
from UnityPy.helpers.TypeTreeNode import TypeTreeNode
from lazy_unity_bundle import BufferedNodeStream, EndianBinaryReader, MetadataReader

_ORIGINAL = getattr(ObjectReader.from_reader, "__func__", None)
_ORIGINAL_PEEK = ObjectReader.peek_name
_ORIGINAL_NODE_READER = lazy_unity_bundle.node_reader
_ORIGINAL_READ_FILES = lazy_unity_bundle.LazyBundle.read_files
_ORIGINAL_READ_BLOCKS = lazy_unity_bundle.Blocks.read_at
_ORIGINAL_TYPE_TREE = getattr(TypeTreeNode.parse_blob, "__func__", None)
_RECORDS = {endian: Struct(endian + "qqIi") for endian in ("<", ">")}
_RECORD_BYTES = _RECORDS["<"].size
_CLASS_BY_ID = ClassIDType._value2member_map_.get
_INT32 = {endian: Struct(endian + "i") for endian in ("<", ">")}
_TREE_HEADER = {endian: Struct(endian + "ii") for endian in ("<", ">")}
_TREE_NODE_BYTES = Struct("<hBBIIiiiQ").size
_TREE_CACHE_ENTRIES = 256
_TREE_CACHE_BLOB_BYTES = 32 * 1024
# Enough of the serialized GameObject tree to determine the name offset.
# Compare actual node layouts, including alignment flags, once per tree.
_GAME_OBJECT_PREFIX = (
    (0, 'GameObject', 'Base', -1, 32768),
    (1, 'vector', 'm_Component', -1, 268468289),
    (2, 'Array', 'Array', -1, 268451905),
    (3, 'int', 'size', 4, 268435521),
    (3, 'ComponentPair', 'data', 12, 268435521),
    (4, 'PPtr<Component>', 'component', 12, 268435521),
    (5, 'int', 'm_FileID', 4, 268435521),
    (5, 'SInt64', 'm_PathID', 8, 268435521),
    (1, 'unsigned int', 'm_Layer', 4, 0),
    (1, 'string', 'm_Name', -1, 32768),
    (2, 'Array', 'Array', -1, 16385),
    (3, 'int', 'size', 4, 1),
    (3, 'char', 'data', 1, 1),
)
_NAME_LAYOUTS = {}


class OmittedObject:
    # UnityPy writes one dictionary entry per record inside its constructor.
    # Use a shared, non-integer key so skipped records need no reader or slot.
    path_id = object()
    type = None


class SelectedObjectBundle(lazy_unity_bundle.LazyBundle):
    def __init__(self, reader, environment, name, classes):
        self.object_classes = classes
        super().__init__(reader, environment, name=name)


def load_selected_objects(path, classes):
    """Open a bundle for callers that only resolve objects of known classes."""
    if not install():
        return lazy_unity_bundle.load(path)
    selected = frozenset(classes)
    if not selected or any(type(kind) is not int or kind <= 0 for kind in selected):
        raise ValueError("Invalid UnityFS object selection")
    environment = UnityPy.Environment()
    stream = path.open("rb")
    try:
        bundle = SelectedObjectBundle(EndianBinaryReader(stream), environment, str(path), selected)
        environment.files[str(path)] = bundle
        environment.path = str(path.parent)
        return environment, stream
    except BaseException:
        stream.close()
        raise


class GameObjectName:
    """Object view for a search that only reads m_Name."""

    __slots__ = ("object", "type", "path_id", "m_Name")

    def __init__(self, obj):
        self.object = obj
        self.type = obj.type
        self.path_id = obj.path_id

    def read(self):
        try:
            name = self.object.peek_name()
        except (AttributeError, ValueError, TypeError, IndexError):
            name = None
        if not isinstance(name, str):
            name = self.object.read().m_Name
        self.m_Name = name
        return self

    def read_typetree(self):
        return self.object.read_typetree()


class NameScan:
    """Keep indexed reads on original objects while scanning GameObject names."""

    def __init__(self, objects):
        self.objects = objects

    def __getitem__(self, object_id):
        return self.objects[object_id]

    def values(self):
        for obj in self.objects.values():
            if obj.type == 1:
                yield GameObjectName(obj)


def read_object(cls, assets_file, reader):
    header = assets_file.header
    if header.version != 22:
        return _ORIGINAL(cls, assets_file, reader)
    if isinstance(reader, MetadataReader):
        stream = reader.stream
        position = stream.position
        aligned = position if assets_file.big_id_enabled else (position + 3) & ~3
        end = aligned + _RECORD_BYTES
        prefix = reader.prefix
        if end > len(prefix):
            return _ORIGINAL(cls, assets_file, reader)
        fields = _RECORDS[reader.endian].unpack_from(prefix, aligned)
        stream.position = end
    elif isinstance(stream := getattr(reader, 'stream', None), BufferedNodeStream):
        # Large metadata tables can exceed the eager prefix budget. Keep the
        # bounded reader and decode a complete record with one buffered read.
        base = reader.BaseOffset
        position = stream.tell() - base
        aligned = position if assets_file.big_id_enabled else (position + 3) & ~3
        if aligned + _RECORD_BYTES > stream.raw.size - base:
            return _ORIGINAL(cls, assets_file, reader)
        if aligned != position:
            reader.Position = aligned
        # The record is fixed at 24 bytes and already inside the node.
        data = BufferedReader.read(stream, _RECORD_BYTES)
        if len(data) != _RECORD_BYTES:
            reader.Position = position
            return _ORIGINAL(cls, assets_file, reader)
        fields = _RECORDS[reader.endian].unpack(data)
    else:
        return _ORIGINAL(cls, assets_file, reader)
    path_id, relative_start, byte_size, type_id = fields
    byte_start = relative_start + header.data_offset
    serialized_type = assets_file.types[type_id]
    class_id = serialized_type.class_id
    class_type = _CLASS_BY_ID(class_id)
    if class_type is None:
        class_type = ClassIDType(class_id)
    return cls(assets_file, reader, path_id, type_id, serialized_type, class_id,
               class_type, byte_start, byte_size, None, None)


def read_selected_object(cls, assets_file, reader):
    if assets_file.header.version == 22 and isinstance(reader, MetadataReader):
        selected = getattr(reader.blocks, "_object_classes", None)
        position = reader.stream.position
        aligned = position if assets_file.big_id_enabled else (position + 3) & ~3
        end = aligned + _RECORD_BYTES
        if selected is not None and end <= len(reader.prefix):
            type_id, = _INT32[reader.endian].unpack_from(reader.prefix, end - 4)
            if 0 <= type_id < len(assets_file.types) and assets_file.types[type_id].class_id not in selected:
                reader.stream.position = end
                return OmittedObject
    return read_object(cls, assets_file, reader)


def read_metadata_node(blocks, node, _available):
    # UnityPy finishes the previous serialized file before opening another node.
    # Its objects retain their readers, so keep the lazy stream and cursor alive
    # while returning the temporary prefix budget to the next file.
    previous = getattr(blocks, '_active_metadata_reader', None)
    if previous is not None:
        previous.prefix = b''
    reader, size = _ORIGINAL_NODE_READER(blocks, node, lazy_unity_bundle.MAX_METADATA)
    blocks._active_metadata_reader = reader if size else None
    return reader, 0


def read_metadata_files(bundle, blocks, nodes):
    # Release graphs left by earlier bundles before temporarily pausing the
    # collector. This pass retains the new object table until parsing finishes.
    pause_gc = gc.isenabled() and blocks.length >= lazy_unity_bundle.MAX_METADATA
    if pause_gc:
        gc.collect()
        gc.disable()
    blocks._metadata_trees = {}
    selected = getattr(bundle, "object_classes", None)
    if selected is not None:
        blocks._object_classes = selected
        reader_factory = ObjectReader.__dict__["from_reader"]
        ObjectReader.from_reader = classmethod(read_selected_object)
    try:
        _ORIGINAL_READ_FILES(bundle, blocks, nodes)
        if selected is not None:
            for file in bundle.files.values():
                objects = getattr(file, "objects", None)
                if isinstance(objects, dict):
                    objects.pop(OmittedObject.path_id, None)
    finally:
        del blocks._metadata_trees
        if selected is not None:
            ObjectReader.from_reader = reader_factory
            del blocks._object_classes
        if pause_gc:
            gc.enable()


def read_blocks(blocks, start, count):
    # Object bodies usually fit in one block. Return the bytes directly
    # instead of assembling a list. The original reader handles boundaries
    # and populates the compressed-block cache on a miss.
    if 0 <= start and 0 < count <= lazy_unity_bundle.MAX_READ and start + count <= blocks.length:
        index = bisect_right(blocks.starts, start) - 1
        at, _, size, flags = blocks.blocks[index]
        offset = start - blocks.starts[index]
        end = offset + count
        if end <= size:
            if flags & 63 == 0:
                blocks.reader.Position = at + offset
                result = blocks.reader.read_bytes(count)
                if len(result) != count:
                    raise ValueError("Truncated UnityFS stored block")
                return result
            data = blocks.cache.get(index)
            if data is not None:
                blocks.cache.move_to_end(index)
                return data[offset:end]
    return _ORIGINAL_READ_BLOCKS(blocks, start, count)


def read_type_tree(cls, reader, version):
    if cls is TypeTreeNode and version == 22 and isinstance(reader, MetadataReader):
        cache = getattr(reader.blocks, "_metadata_trees", None)
        start = reader.Position
        if cache is not None and start + 8 <= len(reader.prefix):
            endian = reader.endian
            nodes, strings = _TREE_HEADER[endian].unpack_from(reader.prefix, start)
            end = start + 8 + nodes * _TREE_NODE_BYTES + strings
            # Compare the complete encoded layout, including local strings.
            if (nodes > 0 and strings >= 0
                    and end <= min(len(reader.prefix), start + _TREE_CACHE_BLOB_BYTES)):
                key = (endian, reader.prefix[start:end])
                tree = cache.get(key)
                if tree is not None:
                    reader.Position = end
                    return tree
                tree = _ORIGINAL_TYPE_TREE(cls, reader, version)
                if len(cache) < _TREE_CACHE_ENTRIES and reader.Position == end:
                    cache[key] = tree
                return tree
    return _ORIGINAL_TYPE_TREE(cls, reader, version)


def known_game_object_tree(tree):
    entry = _NAME_LAYOUTS.get(id(tree))
    if entry is not None:
        return entry[1]
    # Include the complete first three subtrees so an unexpected field that
    # changes the component size or name offset takes the normal reader path.
    def flatten(node, level):
        yield (level, node.m_Type, node.m_Name, node.m_ByteSize, node.m_MetaFlag)
        for child in node.m_Children:
            yield from flatten(child, level + 1)

    fields = (field for child in tree.m_Children[:3] for field in flatten(child, 1))
    root = (0, tree.m_Type, tree.m_Name, tree.m_ByteSize, tree.m_MetaFlag)
    prefix = (root,) + tuple(islice(fields, len(_GAME_OBJECT_PREFIX)))
    matches = prefix == _GAME_OBJECT_PREFIX
    if len(_NAME_LAYOUTS) < 256:
        _NAME_LAYOUTS[id(tree)] = (tree, matches)
    return matches


def peek_name(obj):
    if (obj.class_id != 1 or not 12 <= obj.byte_size <= 64 * 1024
            or obj.serialized_type is None or obj.serialized_type.node is None
            or not known_game_object_tree(obj.serialized_type.node)):
        return _ORIGINAL_PEEK(obj)
    obj.reset()
    data = obj.reader.read_bytes(obj.byte_size)
    if len(data) != obj.byte_size:
        return _ORIGINAL_PEEK(obj)
    int32 = _INT32[obj.reader.endian]
    components, = int32.unpack_from(data)
    length_at = 8 + 12 * components
    if components < 0 or length_at + 4 > len(data):
        return _ORIGINAL_PEEK(obj)
    length, = int32.unpack_from(data, length_at)
    start = length_at + 4
    if length < 0 or ((start + length + 3) & ~3) > len(data):
        return _ORIGINAL_PEEK(obj)
    try:
        return data[start:start + length].decode('utf-8')
    except UnicodeDecodeError:
        return _ORIGINAL_PEEK(obj)


def install():
    """Enable only the reviewed reader API, once per metadata process."""
    if UnityPy.__version__ != "1.25.3" or _ORIGINAL is None or _ORIGINAL_TYPE_TREE is None:
        return False
    current = getattr(ObjectReader.from_reader, "__func__", None)
    current_tree = getattr(TypeTreeNode.parse_blob, "__func__", None)
    original_peek = ObjectReader.peek_name
    if (current not in (_ORIGINAL, read_object)
            or current_tree not in (_ORIGINAL_TYPE_TREE, read_type_tree)
            or original_peek not in (_ORIGINAL_PEEK, peek_name)
            or lazy_unity_bundle.node_reader not in (_ORIGINAL_NODE_READER, read_metadata_node)
            or lazy_unity_bundle.LazyBundle.read_files not in (_ORIGINAL_READ_FILES, read_metadata_files)
            or lazy_unity_bundle.Blocks.read_at not in (_ORIGINAL_READ_BLOCKS, read_blocks)):
        return False
    if current is _ORIGINAL:
        ObjectReader.from_reader = classmethod(read_object)
    if read_typetree_boost and original_peek is _ORIGINAL_PEEK:
        ObjectReader.peek_name = peek_name
    lazy_unity_bundle.node_reader = read_metadata_node
    lazy_unity_bundle.LazyBundle.read_files = read_metadata_files
    lazy_unity_bundle.Blocks.read_at = read_blocks
    TypeTreeNode.parse_blob = classmethod(read_type_tree)
    return True
