#include <vector>
#include "Point.hpp"

namespace geom {
    class Polygon {
    public:
        Polygon(std::vector<Point> pts);
        double area() const;

    private:
        std::vector<Point> points_;
    };
}
