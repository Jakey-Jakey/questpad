"""Madgwick AHRS algorithm for 3DoF orientation fusion in pure Python.
Computes unit quaternions [w, x, y, z] from 3-axis accelerometer and 3-axis gyroscope.
"""
import math


class MadgwickAHRS:
    def __init__(self, sample_period=1.0 / 500.0, beta=0.1):
        self.sample_period = sample_period
        self.beta = beta  # algorithm gain
        # Internal quaternion [w, x, y, z]
        self.q = [1.0, 0.0, 0.0, 0.0]
        # Reference calibration quaternion for recentering
        self.q_ref = [1.0, 0.0, 0.0, 0.0]

    def reset(self):
        self.q = [1.0, 0.0, 0.0, 0.0]
        self.q_ref = [1.0, 0.0, 0.0, 0.0]

    def recenter(self):
        """Zero orientation relative to current pose."""
        self.q_ref = list(self.q)

    def get_calibrated_quat(self):
        """Returns relative quaternion [x, y, z, w] suitable for Three.js."""
        # q_rel = q_ref^-1 * q
        # conjugate of unit quaternion [w, x, y, z] is [w, -x, -y, -z]
        rw, rx, ry, rz = self.q_ref[0], -self.q_ref[1], -self.q_ref[2], -self.q_ref[3]
        qw, qx, qy, qz = self.q[0], self.q[1], self.q[2], self.q[3]

        cw = rw * qw - rx * qx - ry * qy - rz * qz
        cx = rw * qx + rx * qw + ry * qz - rz * qy
        cy = rw * qy - rx * qz + ry * qw + rz * qx
        cz = rw * qz + rx * qy - ry * qx + rz * qw

        # Return Three.js format [x, y, z, w]
        return [cx, cy, cz, cw]

    def update(self, gx, gy, gz, ax, ay, az, dt=None):
        """Update quaternion with gyroscope (rad/s) and accelerometer.
        dt in seconds (if None, sample_period is used).
        """
        dt = dt if dt is not None and dt > 0 else self.sample_period
        q1, q2, q3, q4 = self.q

        # Rate of change of quaternion from gyroscope
        q_dot1 = 0.5 * (-q2 * gx - q3 * gy - q4 * gz)
        q_dot2 = 0.5 * (q1 * gx + q3 * gz - q4 * gy)
        q_dot3 = 0.5 * (q1 * gy - q2 * gz + q4 * gx)
        q_dot4 = 0.5 * (q1 * gz + q2 * gy - q3 * gx)

        # Compute feedback only if accelerometer measurement valid (avoids NaN)
        norm_a = math.sqrt(ax * ax + ay * ay + az * az)
        if norm_a > 1e-4:
            ax /= norm_a
            ay /= norm_a
            az /= norm_a

            # Auxiliary variables to avoid repeated arithmetic
            _2q1 = 2.0 * q1
            _2q2 = 2.0 * q2
            _2q3 = 2.0 * q3
            _2q4 = 2.0 * q4
            _4q1 = 4.0 * q1
            _4q2 = 4.0 * q2
            _4q3 = 4.0 * q3
            _8q2 = 8.0 * q2
            _8q3 = 8.0 * q3
            q1q1 = q1 * q1
            q2q2 = q2 * q2
            q3q3 = q3 * q3
            q4q4 = q4 * q4

            # Gradient decent algorithm corrective step
            s1 = _4q1 * q3q3 + _2q3 * ax + _4q1 * q2q2 - _2q2 * ay
            s2 = _4q2 * q4q4 - _2q4 * ax + 4.0 * q1q1 * q2 - _2q1 * ay - _4q2 + _8q2 * q2q2 + _8q2 * q3q3 + _4q2 * az
            s3 = 4.0 * q1q1 * q3 + _2q1 * ax + _4q3 * q4q4 - _2q4 * ay - _4q3 + _8q3 * q2q2 + _8q3 * q3q3 + _4q3 * az
            s4 = 4.0 * q2q2 * q4 - _2q2 * ax + 4.0 * q3q3 * q4 - _2q3 * ay

            norm_s = math.sqrt(s1 * s1 + s2 * s2 + s3 * s3 + s4 * s4)
            if norm_s > 1e-6:
                s1 /= norm_s
                s2 /= norm_s
                s3 /= norm_s
                s4 /= norm_s

                # Apply feedback step
                q_dot1 -= self.beta * s1
                q_dot2 -= self.beta * s2
                q_dot3 -= self.beta * s3
                q_dot4 -= self.beta * s4

        # Integrate rate of change of quaternion
        q1 += q_dot1 * dt
        q2 += q_dot2 * dt
        q3 += q_dot3 * dt
        q4 += q_dot4 * dt

        # Normalise quaternion
        norm_q = math.sqrt(q1 * q1 + q2 * q2 + q3 * q3 + q4 * q4)
        if norm_q > 1e-6:
            self.q = [q1 / norm_q, q2 / norm_q, q3 / norm_q, q4 / norm_q]
