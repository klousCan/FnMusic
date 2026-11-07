// Run the upstream fixture on loopback with simulated authorization already granted.
const http = require('node:http')
const path = require('node:path')

if (!process.argv[2]) {
  throw new Error('Pass the path to the upstream dev/mock-server.js')
}

process.env.PORT = '5667'
process.env.MOCK_SHOWCASE = '1'
const createServer = http.createServer
const listen = http.Server.prototype.listen

http.createServer = function (handler) {
  return createServer.call(this, (request, response) => {
    if (request.url.startsWith('/signin?')) {
      request.headers.cookie = 'fnos_session=1'
    }
    return handler(request, response)
  })
}

http.Server.prototype.listen = function (port, callback) {
  return listen.call(this, Number(port), '127.0.0.1', callback)
}

require(path.resolve(process.argv[2]))
