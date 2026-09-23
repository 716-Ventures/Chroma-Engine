<?php
$hostHeader = $_SERVER['HTTP_HOST'] ?? ($_SERVER['SERVER_NAME'] ?? '127.0.0.1');
$host = parse_url('http://' . $hostHeader, PHP_URL_HOST);
if (!is_string($host) || !preg_match('/^[A-Za-z0-9.-]+$/', $host)) {
    $host = $_SERVER['SERVER_ADDR'] ?? '127.0.0.1';
}
$connection = @fsockopen('127.0.0.1', 32410, $errorNumber, $errorText, 1);
if ($connection !== false) {
    fclose($connection);
    header('Location: http://' . $host . ':32410/');
    exit;
}
http_response_code(503);
header('Content-Type: text/plain; charset=utf-8');
$status = @file_get_contents(__DIR__ . '/startup-status.txt');
$status = is_string($status) ? trim($status) : 'no-startup-status';
if (!preg_match('/^[a-z-]{1,48}$/', $status)) {
    $status = 'invalid-startup-status';
}
echo "Chroma Server is installed but is not accepting connections on port 32410.\n";
echo "Startup status: " . $status . "\n";
