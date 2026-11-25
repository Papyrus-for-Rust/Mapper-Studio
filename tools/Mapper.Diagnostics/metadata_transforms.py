"""Fast matrix products for staged metadata extraction."""

AFFINE_ROW = (0., 0., 0., 1.)


def multiply(a, b):
    """Keep the writer's product and CPython summation order for each element."""
    b0, b1, b2, b3 = b
    if a[3] == b3 == AFFINE_ROW:
        result = tuple(
            (
                sum((x*b0[0], y*b1[0], z*b2[0])),
                sum((x*b0[1], y*b1[1], z*b2[1])),
                sum((x*b0[2], y*b1[2], z*b2[2])),
                sum((x*b0[3], y*b1[3], z*b2[3], w)),
            )
            for x, y, z, w in a[:3]
        ) + (AFFINE_ROW,)
    else:
        result = tuple(
            (
                sum((x*b0[0], y*b1[0], z*b2[0], w*b3[0])),
                sum((x*b0[1], y*b1[1], z*b2[1], w*b3[1])),
                sum((x*b0[2], y*b1[2], z*b2[2], w*b3[2])),
                sum((x*b0[3], y*b1[3], z*b2[3], w*b3[3])),
            )
            for x, y, z, w in a
        )
    if any(not -1_000_000 <= value <= 1_000_000 for row in result for value in row):
        raise ValueError("Hierarchy transform exceeds inspection bounds")
    return result
